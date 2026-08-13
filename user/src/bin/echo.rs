//! Print the arguments, separated by spaces.

#![no_std]
#![no_main]

use alexos_user::{Args, nr, print, println, write};

alexos_user::entry!(main);

fn main(args: Args) -> i32 {
    // Skip argv[0], the program name.
    for (i, arg) in args.skip(1).enumerate() {
        if i > 0 {
            print!(" ");
        }
        // Written as raw bytes: an argument is not guaranteed to be UTF-8 and
        // mangling it into replacement characters would be worse than passing
        // it through unchanged.
        write(nr::STDOUT, arg);
    }
    println!();
    0
}
