## rust-hotswap
A library for hotswapping running code with minimal effort.

Beware that the library is a completely unsafe prototype for now, and it will probably crash a lot.

## Usage
- Using a nightly rust, import the plugin `hotswap`.
- Annotate the functions you want to hotswap with the `#[hotswap]` modifier.
- Add the `#![hotswap_header]` to the top of your program.
- Add the `hotswap_start!()` macro to the entry point of your program, before you call any hotswapped functions.

## Current Limitations
- Changing hotswapped function signatures **WILL** result in a segfault.
- Requires extra dependency in the user application.
- Leaks a dynamic library on each swap.

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

    loop {
        test(123);
        sleep(Duration::from_millis(2000));
    }
}

```

And that is it!
From there you can
```
> cargo run
     Running `target\debug\hotswaptest.exe`
Foo: 123
Foo: 123
Foo: 123
```

and if you edit the printing code, changing it to
```rust
    println!("Bar: {}", test);
```
and recompiling the code on another terminal (or putting the running task in the background)
```
> cargo build --lib
   Compiling hotswapdemo v0.1.0[...]
> fg
Foo: 123
Foo: 123
Bar: 123
Bar: 123
```
the running function will update automatically to the latest version without restarting the executable!
