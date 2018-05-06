#![feature(plugin, const_fn)]
#![plugin(hotswap)]
#![hotswap_header]

use std::io;

#[hotswap]
fn test() -> String {
    #[cfg(not(feature="hotswap_toggle"))]
    let result = "first".to_string();
    #[cfg(feature="hotswap_toggle")]
    let result = "second".to_string();

    return result;
}


fn main() {
    unsafe { hotswap_start!() }

    println!("ready");

    let mut buffer = String::new();

    io::stdin().read_line(&mut buffer).unwrap();
    println!("{}", test());

    io::stdin().read_line(&mut buffer).unwrap();
    println!("{}", test());
}
