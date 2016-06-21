## rust-hotswap
A library for hotswapping running code with minimal effort.

Beware that the library is a completely unsafe prototype for now, and it will probably crash a lot.

## Usage
- Using a nightly rust, import the plugin `hotswap`.
- Annotate the functions you want to hotswap with the `#[hotswap]` modifier.
- Add the `#![hotswap_header]` to the top of your program.
- Add the `hotswap_start!()` macro to the entry point of your program, before you call any hotswapped functions.
- Add a `dylib` build with the same project name and path to your `Cargo.toml`.

## Current Limitations
- Changing hotswapped function signatures **WILL** result in a segfault.
- Requires an extra dependency in the user application.
- Leaks a dynamic library on each swap.
- Probably many others that I don't know about!

## Example
```toml
# Cargo.toml

[package]
name = "hotswapdemo"
version = "0.1.0"

[lib]
name = "hotswapdemo"
crate-type = ["dylib"]
path = "src/main.rs"

[dependencies]
hotswap = "*"    # libloading is a required
libloading = "*" # dependency for now
```

```rust
// main.rs

#![feature(plugin)]
#![plugin(hotswap)]
#![hotswap_header]

use std::thread::sleep;
use std::time::Duration;

#[hotswap]
fn test(test: i32) -> () {
    println!("Foo: {}", test);
}

fn main() {
    hotswap_start!();

    let mut i = 1;
    loop {
        test(i);
        i += 1;
        sleep(Duration::from_millis(2000));
    }
}

```

And that is it!

From there you can
```
> cargo run
     Running `target/debug/hotswapdemo`
Foo: 1
Foo: 2
Foo: 3
```

once it is running, you can edit the printing code, e.g.
```rust
    println!("Bar: {} :)", test);
```
and once you recompile the code on another terminal (or on the same one using background)
```
> cargo build --lib
   Compiling hotswapdemo v0.1.0 [...]
> fg
Foo: 7
Foo: 8
Bar: 9 :)
Bar: 10 :)
```
the running code will update without restarting the binary or losing state!
