# Rejected archive attempt retained for provenance

- Rejected proposal directory: `/private/tmp/aethyme-t01-doc-proposal-20261016-010500`
- Rejected proposal-manifest SHA-256: `e59e1bcae2036376aa3caf88082b267e1ed719600a240e07dd6e9618927a683f`
- Rejected archive SHA-256: `4764c824ae7f9e6817ab6a31d87d362890af64fa522bb40baab3202340b94592`
- Inventory: `rejected-archive-inventory.tsv` (754 tar members, 723 regular entries, 377 AppleDouble `._*` sidecars, 31 directory entries).
- Independent review rejected that wrapper because the macOS tar writer included AppleDouble sidecars. The 346 original package files still matched their frozen source bytes; the sidecars were archive-only metadata.
- The rejected proposal and archive remain unchanged at the path above and are excluded from this new archive.
- The new archive is written with Python `tarfile` in USTAR format and contains exactly the 346 original regular files, with no directory records, PAX records, links, duplicates, traversal paths, AppleDouble entries, or unlisted files.
- All original 346 file byte streams were compared against the source package during archive verification.
