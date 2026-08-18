fn main() {
    // Man page is at man/ymx.1 in the crate source directory.
    // Cargo does not natively install man pages; users can view it with:
    //   man ./man/ymx.1
    // or copy it to their man1/manpages directory.
    println!("cargo:warning=ymx-cli does not natively install man pages.");
    println!("cargo:warning=View the man page with: man ./man/ymx.1");
}
