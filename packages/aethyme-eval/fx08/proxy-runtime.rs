// Trusted oracle-side transport. The subject is a separate sandboxed process.
fn rpc(op:&str,args:serde_json::Value)->serde_json::Value{
    use std::io::{Read,Write};
    use std::process::{Command,Stdio};
    use std::time::{Duration,Instant};
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    unsafe extern "C"{fn pipe(fds:*mut i32)->i32;fn dup2(old:i32,new:i32)->i32;fn close(fd:i32)->i32;}
    let binary=std::env::var("FX08_SUBJECT").unwrap();
    let policy=std::env::var("FX08_SUBJECT_POLICY").unwrap();
    let mut fds=[-1;2];assert_eq!(unsafe{pipe(fds.as_mut_ptr())},0);
    let reader=unsafe{std::fs::File::from_raw_fd(fds[0])};
    let writer=unsafe{std::fs::File::from_raw_fd(fds[1])};
    let mut command=Command::new("/usr/bin/sandbox-exec");
    command.args(["-p",&policy,&binary]).env_clear().env("PATH","/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir(std::path::Path::new(&binary).parent().unwrap())
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::inherit());
    unsafe{command.pre_exec(move||{close(fds[0]);if dup2(fds[1],3)<0{return Err(std::io::Error::last_os_error());}if fds[1]!=3{close(fds[1]);}Ok(())});}
    let mut child=command.spawn().unwrap();drop(writer);
    child.stdin.take().unwrap().write_all(serde_json::to_string(&serde_json::json!({"op":op,"args":args})).unwrap().as_bytes()).unwrap();
    let stdout=reader;
    let (tx,rx)=std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move||{let mut bytes=Vec::new();let read=stdout.take(65537).read_to_end(&mut bytes);let _=tx.send((read,bytes));});
    let deadline=Instant::now()+Duration::from_secs(3);
    let mut output=None;
    let status=loop{
        if let Ok(value)=rx.try_recv(){output=Some(value);}
        if output.as_ref().is_some_and(|(_,bytes)|bytes.len()>65536) || Instant::now()>=deadline{
            let _=child.kill();let _=child.wait();panic!("subject API deadline/output allowance exhausted");
        }
        if let Some(status)=child.try_wait().unwrap(){break status;}
        std::thread::sleep(Duration::from_millis(5));
    };
    let (read,bytes)=output.unwrap_or_else(||rx.recv_timeout(Duration::from_secs(1)).unwrap());
    read.unwrap();assert!(bytes.len()<=65536 && status.success(),"subject API did not complete: {status}");
    let value:serde_json::Value=serde_json::from_slice(&bytes).expect("subject output must be one complete API reply, never printed test results");
    assert_eq!(value["schema"],"aethyme.fx08.api-reply/v1");
    value.get("result").expect("API result").clone()
}
