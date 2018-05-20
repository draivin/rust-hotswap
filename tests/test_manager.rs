use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

#[test]
fn hotswap_test_crate() {
    let mut child = Command::new("cargo")
        .arg("run")
        .current_dir("tests/hotswap-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to start cargo run");

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stdin = child.stdin.take().unwrap();

    println!("building bin");
    let mut output = String::new();
    stdout.read_line(&mut output).unwrap();
    println!("{}", output.trim());

    println!("running test crate");
    stdin.write(b"echo\n").unwrap();
    output.clear();
    stdout.read_line(&mut output).unwrap();

    println!("{}", output.trim());
    assert!(output.trim() == "first");

    println!("building lib");
    Command::new("cargo")
        .args(&["build", "--lib", "--features", "hotswap_toggle"])
        .current_dir("tests/hotswap-test")
        .stdout(Stdio::null())
        .status()
        .expect("Failed to build lib");

    // Wait while hotswap reads the freshly compiled library.
    sleep(Duration::from_millis(5000));

    stdin.write(b"echo\n").unwrap();
    output.clear();
    stdout.read_line(&mut output).unwrap();

    println!("{}", output.trim());
    assert!(output.trim() == "second");

    child.wait().unwrap();
}
