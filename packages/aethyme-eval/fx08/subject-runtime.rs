// Public API transport only. No private oracle exists in this compiler scope.
fn main(){
    use std::io::Read;
    let mut input=String::new();std::io::stdin().take(65537).read_to_string(&mut input).unwrap();
    assert!(input.len()<=65536);
    let request:serde_json::Value=serde_json::from_str(&input).unwrap();
    let op=request["op"].as_str().unwrap();let a=&request["args"];
    let result=dispatch(op,a);
    use std::os::fd::FromRawFd;
    use std::io::Write;
    // Trusted adapter only: the separately compiled subject forbids unsafe code.
    // The sandbox denies reopening /dev/fd and spawning other programs.
    let mut channel=unsafe{std::fs::File::from_raw_fd(3)};
    channel.write_all(serde_json::to_string(&serde_json::json!({"schema":"aethyme.fx08.api-reply/v1","result":result})).unwrap().as_bytes()).unwrap();
}
