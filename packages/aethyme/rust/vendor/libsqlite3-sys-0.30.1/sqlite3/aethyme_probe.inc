/*
** Aethyme lock-safe, temporary metadata probes for the bundled Unix VFS.
**
** Probe descriptors are retained until the Unix VFS can close them without
** dropping process-associated SQLite locks. Rust owns opaque tokens, never
** the descriptors themselves.
*/

#include <stdint.h>
#include <stddef.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <limits.h>

#define AETHYME_PROBE_SLOT_COUNT 128
#define AETHYME_PROBE_TOKEN_COUNT 128
#define AETHYME_PROBE_HANDLE_INDEX_BITS 8
#define AETHYME_PROBE_HANDLE_INDEX_MASK UINT64_C(0xff)
#define AETHYME_PROBE_HANDLE_GENERATION_MAX (UINT64_MAX >> 8)
#define AETHYME_PROBE_ABI_V1 1u
/* Private ABI status; Rust maps this to a probe-capacity refusal, not disk-full. */
#define AETHYME_PROBE_CAPACITY_REFUSAL 0x41595001

typedef char aethymeProbeDevFitsU64[
  sizeof(dev_t)<=sizeof(uint64_t) ? 1 : -1
];
typedef char aethymeProbeInoFitsU64[
  sizeof(ino_t)<=sizeof(uint64_t) ? 1 : -1
];
typedef char aethymeProbeSizeFitsU64[
  sizeof(off_t)<=sizeof(uint64_t) ? 1 : -1
];
typedef char aethymeProbeNlinkFitsU64[
  sizeof(nlink_t)<=sizeof(uint64_t) ? 1 : -1
];
typedef char aethymeProbeUidFitsU64[
  sizeof(uid_t)<=sizeof(uint64_t) ? 1 : -1
];
typedef char aethymeProbeModeFitsU64[
  sizeof(mode_t)<=sizeof(uint64_t) ? 1 : -1
];

enum {
  AETHYME_PROBE_MAIN_DATABASE = 1,
  AETHYME_PROBE_SIDECAR = 2
};

enum {
  AETHYME_PROBE_SLOT_FREE = 0,
  AETHYME_PROBE_SLOT_RESERVED,
  AETHYME_PROBE_SLOT_OPEN,
  AETHYME_PROBE_SLOT_PENDING,
  AETHYME_PROBE_SLOT_UNKNOWN,
  AETHYME_PROBE_SLOT_RETIRED
};

/*
** Rust initializes abi_version and struct_size before calling acquire. C
** leaves the output untouched on an invalid ABI, before any open. On every
** ABI-valid error, handle is zero.
*/
typedef struct AethymeProbeResultV1 AethymeProbeResultV1;
struct AethymeProbeResultV1 {
  uint32_t abi_version;
  uint32_t struct_size;
  uint64_t handle;
  uint64_t dev;
  uint64_t ino;
  uint64_t size;
  uint64_t nlink;
  uint64_t uid;
  uint64_t mode;
};

#if SQLITE_THREADSAFE

typedef struct AethymeProbeSlot AethymeProbeSlot;
struct AethymeProbeSlot {
  int state;
  int fd;
  dev_t dev;
  ino_t ino;
  uint32_t generation;
  uint32_t references;
  int pending_close_fd;
  int pending_close_errno;
};

typedef struct AethymeProbeTokenCell AethymeProbeTokenCell;
struct AethymeProbeTokenCell {
  uint64_t generation;
  uint32_t slot_generation;
  uint16_t slot_index;
  uint8_t active;
  uint8_t retired;
};

typedef struct AethymeProbeCloseLog AethymeProbeCloseLog;
struct AethymeProbeCloseLog {
  int fd;
  int error;
};

/*
** All fields below are protected by unixBigLock. The fixed token pool bounds
** simultaneous leases; the fixed slot table bounds physical descriptors,
** including UNKNOWN and RETIRED slots. Static storage avoids allocation while
** the SQLite global mutex is held.
*/
static AethymeProbeSlot aAethymeProbeSlots[AETHYME_PROBE_SLOT_COUNT];
static AethymeProbeTokenCell aAethymeProbeTokens[AETHYME_PROBE_TOKEN_COUNT];
static int aAethymeProbeInitialized = 0;

/* These declarations belong before the three lifecycle hook call sites. */
static void aethymeProbeDrain(void);
static void aethymeProbeDrainLocked(void);
static void aethymeProbeFlushCloseLogs(void);

static void aethymeProbeInitLocked(void){
  int i;
  assert( unixMutexHeld() );
  if( aAethymeProbeInitialized ) return;
  for(i=0; i<AETHYME_PROBE_SLOT_COUNT; i++){
    aAethymeProbeSlots[i].state = AETHYME_PROBE_SLOT_FREE;
    aAethymeProbeSlots[i].fd = -1;
    aAethymeProbeSlots[i].generation = 1;
    aAethymeProbeSlots[i].pending_close_fd = -1;
  }
  for(i=0; i<AETHYME_PROBE_TOKEN_COUNT; i++){
    aAethymeProbeTokens[i].generation = 1;
    aAethymeProbeTokens[i].active = 0;
    aAethymeProbeTokens[i].retired = 0;
  }
  aAethymeProbeInitialized = 1;
}

static uint64_t aethymeProbeHandle(int tokenIndex, uint64_t generation){
  return (generation << AETHYME_PROBE_HANDLE_INDEX_BITS)
       | (uint64_t)(tokenIndex + 1);
}

static int aethymeProbeDecodeHandle(
  uint64_t handle,
  int *pTokenIndex,
  uint64_t *pGeneration
){
  uint64_t indexPlusOne = handle & AETHYME_PROBE_HANDLE_INDEX_MASK;
  if( handle==0 || indexPlusOne==0
   || indexPlusOne>AETHYME_PROBE_TOKEN_COUNT
  ){
    return 0;
  }
  *pTokenIndex = (int)indexPlusOne - 1;
  *pGeneration = handle >> AETHYME_PROBE_HANDLE_INDEX_BITS;
  return *pGeneration!=0;
}

static int aethymeProbeFindFreeSlotLocked(void){
  int i;
  assert( unixMutexHeld() );
  for(i=0; i<AETHYME_PROBE_SLOT_COUNT; i++){
    if( aAethymeProbeSlots[i].state==AETHYME_PROBE_SLOT_FREE ){
      return i;
    }
  }
  return -1;
}

static int aethymeProbeFindFreeTokenLocked(void){
  int i;
  assert( unixMutexHeld() );
  for(i=0; i<AETHYME_PROBE_TOKEN_COUNT; i++){
    if( !aAethymeProbeTokens[i].active && !aAethymeProbeTokens[i].retired ){
      return i;
    }
  }
  return -1;
}

static int aethymeProbeActivateTokenLocked(int tokenIndex, int slotIndex){
  AethymeProbeTokenCell *pToken = &aAethymeProbeTokens[tokenIndex];
  AethymeProbeSlot *pSlot = &aAethymeProbeSlots[slotIndex];
  assert( unixMutexHeld() );
  if( pToken->active || pToken->retired || pToken->generation==0
   || pToken->generation>AETHYME_PROBE_HANDLE_GENERATION_MAX
   || pSlot->references==UINT32_MAX
  ){
    return 0;
  }
  pToken->slot_index = (uint16_t)slotIndex;
  pToken->slot_generation = pSlot->generation;
  pToken->active = 1;
  pSlot->references++;
  pSlot->state = AETHYME_PROBE_SLOT_OPEN;
  return 1;
}

static int aethymeProbeIdentityMatches(
  const struct stat *pStat,
  uint64_t dev,
  uint64_t ino
){
  return (uint64_t)pStat->st_dev==dev && (uint64_t)pStat->st_ino==ino;
}

/*
** This check runs outside unixBigLock. It uses the same bundled finder as
** fillInUnixFile for a MAIN_DB open, and does not call fillInUnixFile or
** publish an inode record. The caller has already verified the exact built-in
** unix VFS. Broker open flags must continue to omit SQLITE_OPEN_AUTOPROXY.
*/
static int aethymeProbeMainMethodSupported(
  sqlite3_vfs *pVfs,
  const char *zPath,
  int fd
){
  unixFile probeFile;
  finder_type *pFinder;
  const sqlite3_io_methods *pMethods;

  if( !pVfs || !pVfs->pAppData || !zPath || fd<0 ) return 0;
  memset(&probeFile, 0, sizeof(probeFile));
  probeFile.h = fd;
  probeFile.pVfs = pVfs;
  probeFile.zPath = zPath;
  pFinder = (finder_type*)pVfs->pAppData;
  pMethods = (*pFinder)(zPath, &probeFile);
  if( pMethods==&posixIoMethods ) return 1;
#if defined(__APPLE__) && SQLITE_ENABLE_LOCKING_STYLE
  if( pMethods==&nfsIoMethods ) return 1;
#endif
  return 0;
}

/*
** Called before taking unixBigLock. sqlite3_vfs_find() may initialize SQLite
** and takes SQLite's main mutex, so it must not run inside unixBigLock.
*/
static sqlite3_vfs *aethymeProbeBuiltinUnixVfs(void){
  sqlite3_vfs *pDefault = sqlite3_vfs_find(0);
  sqlite3_vfs *pUnix = sqlite3_vfs_find("unix");
  if( !unixBigLock || !pDefault || pDefault!=pUnix
   || pDefault->xOpen!=unixOpen
  ){
    return 0;
  }
#if defined(__APPLE__) && SQLITE_ENABLE_LOCKING_STYLE
  if( pDefault->pAppData!=(void*)&autolockIoFinder ) return 0;
#else
  if( pDefault->pAppData!=(void*)&posixIoFinder ) return 0;
#endif
#if defined(SQLITE_PREFER_PROXY_LOCKING) && SQLITE_PREFER_PROXY_LOCKING
  return 0;
#endif
  return pDefault;
}

static int aethymeProbeFillResult(
  AethymeProbeResultV1 *pOut,
  uint64_t handle,
  const struct stat *pStat
){
  if( pStat->st_size<0 ) return 0;
  pOut->handle = handle;
  pOut->dev = (uint64_t)pStat->st_dev;
  pOut->ino = (uint64_t)pStat->st_ino;
  pOut->size = (uint64_t)pStat->st_size;
  pOut->nlink = (uint64_t)pStat->st_nlink;
  pOut->uid = (uint64_t)pStat->st_uid;
  pOut->mode = (uint64_t)pStat->st_mode;
  return 1;
}

static int aethymeProbeCloseLocked(AethymeProbeSlot *pSlot){
  int fd = pSlot->fd;
  int rc;
  int savedErrno = 0;
  assert( unixMutexHeld() );
  assert( pSlot->references==0 );
  assert( pSlot->state==AETHYME_PROBE_SLOT_PENDING
       || pSlot->state==AETHYME_PROBE_SLOT_UNKNOWN );
  rc = close(fd);
  if( rc!=0 ) savedErrno = errno;
  pSlot->fd = -1;
  if( rc!=0 ){
    /* Never retry an ambiguous close: the fd number may already be reused. */
    pSlot->state = AETHYME_PROBE_SLOT_RETIRED;
    pSlot->pending_close_fd = fd;
    pSlot->pending_close_errno = savedErrno;
    return SQLITE_IOERR_CLOSE;
  }
  pSlot->pending_close_fd = -1;
  pSlot->pending_close_errno = 0;
  if( pSlot->generation==UINT32_MAX ){
    pSlot->state = AETHYME_PROBE_SLOT_RETIRED;
  }else{
    pSlot->generation++;
    pSlot->state = AETHYME_PROBE_SLOT_FREE;
  }
  return SQLITE_OK;
}

/*
** Return true if a live SHM file has the target identity. If fstat fails for
** any active hShm, conservatively report an unknown alias and defer the close.
*/
static int aethymeProbeShmAliasOrUnknownLocked(dev_t dev, ino_t ino){
#ifndef SQLITE_OMIT_WAL
  unixInodeInfo *pInode;
  assert( unixMutexHeld() );
  for(pInode=inodeList; pInode; pInode=pInode->pNext){
    unixShmNode *pNode = pInode->pShmNode;
    if( pNode && pNode->nRef>0 && pNode->hShm>=0 ){
      struct stat shmStat;
      if( fstat(pNode->hShm, &shmStat)!=0 ) return 1;
      if( shmStat.st_dev==dev && shmStat.st_ino==ino ) return 1;
    }
  }
  return 0;
#else
  /* A build with WAL omitted has no live unixShmNode descriptors to scan. */
  (void)dev;
  (void)ino;
  return 0;
#endif
}

static int aethymeProbeOpenNoFollow(const char *zPath){
#if defined(O_NOFOLLOW) && defined(O_CLOEXEC) && defined(O_NONBLOCK)
  return open(zPath, O_RDONLY|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK);
#else
  /* Keep unsupported Unix targets buildable, but never weaken no-follow. */
  (void)zPath;
  errno = EINVAL;
  return -1;
#endif
}

static int aethymeProbeDrainSlotLocked(AethymeProbeSlot *pSlot){
  unixInodeInfo *pInode;
  int result = SQLITE_OK;
  int heldInodeMutex = 0;
  assert( unixMutexHeld() );
  if( pSlot->references!=0 ) return SQLITE_OK;
  if( pSlot->state==AETHYME_PROBE_SLOT_UNKNOWN ){
    if( inodeList!=0 ) return SQLITE_OK;
    return aethymeProbeCloseLocked(pSlot);
  }
  if( pSlot->state!=AETHYME_PROBE_SLOT_PENDING ) return SQLITE_OK;

  for(pInode=inodeList; pInode; pInode=pInode->pNext){
    if( pInode->fileId.dev==pSlot->dev
     && pInode->fileId.ino==(u64)pSlot->ino
    ){
      sqlite3_mutex_enter(pInode->pLockMutex);
      heldInodeMutex = 1;
      if( pInode->nLock!=0 ){
        sqlite3_mutex_leave(pInode->pLockMutex);
        return SQLITE_OK;
      }
      break;
    }
  }

  if( aethymeProbeShmAliasOrUnknownLocked(pSlot->dev, pSlot->ino) ){
    if( heldInodeMutex ) sqlite3_mutex_leave(pInode->pLockMutex);
    return SQLITE_OK;
  }

  /* Keep a matching inode pLockMutex held through close. */
  result = aethymeProbeCloseLocked(pSlot);
  if( heldInodeMutex ) sqlite3_mutex_leave(pInode->pLockMutex);
  return result;
}

static void aethymeProbeDrainLocked(void){
  int i;
  assert( unixMutexHeld() );
  aethymeProbeInitLocked();
  for(i=0; i<AETHYME_PROBE_SLOT_COUNT; i++){
    (void)aethymeProbeDrainSlotLocked(&aAethymeProbeSlots[i]);
  }
}

static void aethymeProbeDrain(void){
  assert( sqlite3_mutex_notheld(unixBigLock) );
  unixEnterMutex();
  aethymeProbeDrainLocked();
  unixLeaveMutex();
  aethymeProbeFlushCloseLogs();
}

/*
** A close error is stored in its retired slot while the mutexes are held.
** This routine copies and clears the small fixed log queue under
** unixBigLock, then calls sqlite3_log only after releasing that mutex.
*/
static void aethymeProbeFlushCloseLogs(void){
  AethymeProbeCloseLog aLog[AETHYME_PROBE_SLOT_COUNT];
  int nLog = 0;
  int i;
  unixEnterMutex();
  aethymeProbeInitLocked();
  for(i=0; i<AETHYME_PROBE_SLOT_COUNT; i++){
    AethymeProbeSlot *pSlot = &aAethymeProbeSlots[i];
    if( pSlot->state==AETHYME_PROBE_SLOT_RETIRED
     && pSlot->pending_close_errno!=0
    ){
      aLog[nLog].fd = pSlot->pending_close_fd;
      aLog[nLog].error = pSlot->pending_close_errno;
      nLog++;
      pSlot->pending_close_fd = -1;
      pSlot->pending_close_errno = 0;
    }
  }
  unixLeaveMutex();
  for(i=0; i<nLog; i++){
    sqlite3_log(SQLITE_IOERR_CLOSE,
                "aethyme probe close failed for fd %d (errno %d)",
                aLog[i].fd, aLog[i].error);
  }
}

static int aethymeProbeReleaseLocked(uint64_t handle){
  int tokenIndex;
  uint64_t generation;
  AethymeProbeTokenCell *pToken;
  AethymeProbeSlot *pSlot;
  int drainRc;
  assert( unixMutexHeld() );
  if( !aethymeProbeDecodeHandle(handle, &tokenIndex, &generation) ){
    return SQLITE_MISUSE;
  }
  pToken = &aAethymeProbeTokens[tokenIndex];
  if( !pToken->active || pToken->generation!=generation
   || pToken->slot_index>=AETHYME_PROBE_SLOT_COUNT
  ){
    return SQLITE_MISUSE;
  }
  pSlot = &aAethymeProbeSlots[pToken->slot_index];
  if( pSlot->generation!=pToken->slot_generation
   || pSlot->references==0
  ){
    return SQLITE_MISUSE;
  }

  pToken->active = 0;
  if( pToken->generation==AETHYME_PROBE_HANDLE_GENERATION_MAX ){
    pToken->retired = 1;
  }else{
    pToken->generation++;
  }
  pSlot->references--;
  if( pSlot->references==0
   && pSlot->state!=AETHYME_PROBE_SLOT_UNKNOWN
  ){
    pSlot->state = AETHYME_PROBE_SLOT_PENDING;
  }
  drainRc = aethymeProbeDrainSlotLocked(pSlot);
  return drainRc;
}

SQLITE_API int sqlite3_aethyme_probe_release_v1(uint64_t handle){
  int rc;
  unixEnterMutex();
  aethymeProbeInitLocked();
  rc = aethymeProbeReleaseLocked(handle);
  unixLeaveMutex();
  aethymeProbeFlushCloseLogs();
  return rc;
}

SQLITE_API int sqlite3_aethyme_probe_acquire_v1(
  const char *zPath,
  uint32_t kind,
  AethymeProbeResultV1 *pOut
){
  sqlite3_vfs *pVfs = 0;
  struct stat openedStat;
  int slotIndex = -1;
  int tokenIndex = -1;
  int fd = -1;
  int savedErrno = 0;
  uint64_t handle = 0;
  AethymeProbeSlot *pSlot;
  AethymeProbeTokenCell *pToken;

  if( !pOut ) return SQLITE_MISUSE;
  if( pOut->abi_version!=AETHYME_PROBE_ABI_V1
   || pOut->struct_size!=sizeof(AethymeProbeResultV1)
  ){
    /* No open has occurred; leave invalid-ABI memory untouched. */
    return SQLITE_MISUSE;
  }
  pOut->handle = 0;
  pOut->dev = pOut->ino = pOut->size = 0;
  pOut->nlink = pOut->uid = pOut->mode = 0;
  if( !zPath || zPath[0]==0
   || (kind!=AETHYME_PROBE_MAIN_DATABASE && kind!=AETHYME_PROBE_SIDECAR)
  ){
    return SQLITE_MISUSE;
  }

  /* sqlite3_vfs_find initializes SQLite and acquires its own mutex. */
  pVfs = aethymeProbeBuiltinUnixVfs();
  if( !pVfs ) return SQLITE_CANTOPEN;

  unixEnterMutex();
  aethymeProbeInitLocked();
  /* Reclaim every slot that is already safe before refusing for capacity. */
  aethymeProbeDrainLocked();
  tokenIndex = aethymeProbeFindFreeTokenLocked();
  if( tokenIndex<0 ){
    unixLeaveMutex();
    aethymeProbeFlushCloseLogs();
    return AETHYME_PROBE_CAPACITY_REFUSAL;
  }
  slotIndex = aethymeProbeFindFreeSlotLocked();
  if( slotIndex<0 ){
    unixLeaveMutex();
    aethymeProbeFlushCloseLogs();
    return AETHYME_PROBE_CAPACITY_REFUSAL;
  }
  pSlot = &aAethymeProbeSlots[slotIndex];
  pSlot->state = AETHYME_PROBE_SLOT_RESERVED;
  fd = aethymeProbeOpenNoFollow(zPath);
  if( fd<0 ){
    savedErrno = errno;
    pSlot->state = AETHYME_PROBE_SLOT_FREE;
    pSlot->fd = -1;
    unixLeaveMutex();
    aethymeProbeFlushCloseLogs();
    return sqliteErrorFromPosixError(savedErrno, SQLITE_CANTOPEN);
  }
  if( fstat(fd, &openedStat)!=0 ){
    pSlot->fd = fd;
    pSlot->state = AETHYME_PROBE_SLOT_UNKNOWN;
    pSlot->references = 0;
    aethymeProbeDrainSlotLocked(pSlot);
    unixLeaveMutex();
    aethymeProbeFlushCloseLogs();
    return SQLITE_IOERR_FSTAT;
  }
  pSlot->fd = fd;
  pSlot->dev = openedStat.st_dev;
  pSlot->ino = openedStat.st_ino;
  pSlot->references = 0;
  pSlot->state = AETHYME_PROBE_SLOT_PENDING;
  if( !aethymeProbeActivateTokenLocked(tokenIndex, slotIndex) ){
    /* This token is not visible to Rust; release it through the safe path. */
    (void)aethymeProbeDrainSlotLocked(pSlot);
    unixLeaveMutex();
    aethymeProbeFlushCloseLogs();
    return SQLITE_MISUSE;
  }

  pToken = &aAethymeProbeTokens[tokenIndex];
  handle = aethymeProbeHandle(tokenIndex, pToken->generation);
  unixLeaveMutex();
  aethymeProbeFlushCloseLogs();

  /*
  ** The method finder may dispatch a configured VFS system-call hook. Keep it
  ** outside unixBigLock. The provisional token prevents its fd from closing.
  ** Sidecars intentionally do not use this finder; SQLite opens their WAL and
  ** journal descriptors with UNIXFILE_NOLOCK and opens SHM directly.
  */
  if( kind==AETHYME_PROBE_MAIN_DATABASE
   && !aethymeProbeMainMethodSupported(pVfs, zPath, fd)
  ){
    (void)sqlite3_aethyme_probe_release_v1(handle);
    return SQLITE_CANTOPEN;
  }

  if( !aethymeProbeFillResult(pOut, handle, &openedStat) ){
    pOut->handle = 0;
    (void)sqlite3_aethyme_probe_release_v1(handle);
    return SQLITE_IOERR_FSTAT;
  }
  return SQLITE_OK;
}

/*
** Take a fresh fstat snapshot of the descriptor pinned by an active token.
** This replaces Rust File::metadata() calls at their existing points without
** opening or closing another descriptor. On fstat failure the token remains
** active so Rust can drop it through the ordinary safe release path.
*/
SQLITE_API int sqlite3_aethyme_probe_metadata_v1(
  uint64_t handle,
  AethymeProbeResultV1 *pOut
){
  int tokenIndex;
  uint64_t generation;
  AethymeProbeTokenCell *pToken;
  AethymeProbeSlot *pSlot;
  struct stat snapshot;
  int rc = SQLITE_OK;

  if( !pOut ) return SQLITE_MISUSE;
  if( pOut->abi_version!=AETHYME_PROBE_ABI_V1
   || pOut->struct_size!=sizeof(AethymeProbeResultV1)
  ){
    return SQLITE_MISUSE;
  }
  pOut->handle = 0;
  pOut->dev = pOut->ino = pOut->size = 0;
  pOut->nlink = pOut->uid = pOut->mode = 0;

  unixEnterMutex();
  aethymeProbeInitLocked();
  if( !aethymeProbeDecodeHandle(handle, &tokenIndex, &generation) ){
    unixLeaveMutex();
    return SQLITE_MISUSE;
  }
  pToken = &aAethymeProbeTokens[tokenIndex];
  if( !pToken->active || pToken->generation!=generation
   || pToken->slot_index>=AETHYME_PROBE_SLOT_COUNT
  ){
    unixLeaveMutex();
    return SQLITE_MISUSE;
  }
  pSlot = &aAethymeProbeSlots[pToken->slot_index];
  if( pSlot->generation!=pToken->slot_generation
   || pSlot->references!=1
   || (pSlot->state!=AETHYME_PROBE_SLOT_OPEN
    && pSlot->state!=AETHYME_PROBE_SLOT_PENDING)
  ){
    unixLeaveMutex();
    return SQLITE_MISUSE;
  }
  if( fstat(pSlot->fd, &snapshot)!=0
   || snapshot.st_dev!=pSlot->dev
   || snapshot.st_ino!=pSlot->ino
  ){
    pSlot->state = AETHYME_PROBE_SLOT_UNKNOWN;
    pSlot->dev = 0;
    pSlot->ino = 0;
    rc = SQLITE_IOERR_FSTAT;
  }else if( !aethymeProbeFillResult(pOut, 0, &snapshot) ){
    rc = SQLITE_IOERR_FSTAT;
  }
  unixLeaveMutex();
  if( rc!=SQLITE_OK ){
    pOut->handle = 0;
    pOut->dev = pOut->ino = pOut->size = 0;
    pOut->nlink = pOut->uid = pOut->mode = 0;
  }
  return rc;
}

#else

/* Preserve buildability without SQLite mutexes, but never probe unsafely. */
SQLITE_API int sqlite3_aethyme_probe_acquire_v1(
  const char *zPath,
  uint32_t kind,
  AethymeProbeResultV1 *pOut
){
  if( !pOut ) return SQLITE_MISUSE;
  if( pOut->abi_version!=AETHYME_PROBE_ABI_V1
   || pOut->struct_size!=sizeof(AethymeProbeResultV1)
  ){
    return SQLITE_MISUSE;
  }
  pOut->handle = 0;
  pOut->dev = pOut->ino = pOut->size = 0;
  pOut->nlink = pOut->uid = pOut->mode = 0;
  if( !zPath || zPath[0]==0
   || (kind!=AETHYME_PROBE_MAIN_DATABASE && kind!=AETHYME_PROBE_SIDECAR)
  ){
    return SQLITE_MISUSE;
  }
  return SQLITE_CANTOPEN;
}

SQLITE_API int sqlite3_aethyme_probe_metadata_v1(
  uint64_t handle,
  AethymeProbeResultV1 *pOut
){
  (void)handle;
  if( !pOut
   || pOut->abi_version!=AETHYME_PROBE_ABI_V1
   || pOut->struct_size!=sizeof(AethymeProbeResultV1)
  ){
    return SQLITE_MISUSE;
  }
  pOut->handle = 0;
  pOut->dev = pOut->ino = pOut->size = 0;
  pOut->nlink = pOut->uid = pOut->mode = 0;
  return SQLITE_CANTOPEN;
}

SQLITE_API int sqlite3_aethyme_probe_release_v1(uint64_t handle){
  (void)handle;
  return SQLITE_MISUSE;
}

#endif /* SQLITE_THREADSAFE */

