use std::process::{Command, Stdio, Child};
use std::io::{Write, BufReader, BufRead};
use std::thread::sleep;
use std::time::Duration;

#[test]
fn main() {
    let Child {
        stdin,
        stdout,
        ..
    } = Command::new("cargo")
        .arg("run")
        .current_dir("tests/hotswap-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to build binary");

    let mut stdout = BufReader::new(stdout.unwrap());
    let mut stdin = stdin.unwrap();

    let mut output = String::new();
    while output.trim() != "ready" {
        output.clear();
        stdout.read_line(&mut output).unwrap();
    }

    stdin.write(b"echo\n").unwrap();
    output.clear();
    stdout.read_line(&mut output).unwrap();

    assert!(output.trim() == "first");

    Command::new("cargo")
        .args(&["build", "--lib", "--features", "hotswap_toggle"])
        .current_dir("tests/hotswap-test")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("Failed to build library");

    sleep(Duration::from_millis(5000));

    stdin.write(b"echo\n").unwrap();
    output.clear();
    stdout.read_line(&mut output).unwrap();

    assert!(output.trim() == "second");
}
