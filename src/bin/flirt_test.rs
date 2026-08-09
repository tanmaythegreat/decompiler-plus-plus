fn main() {
    let sigs = mini_decompiler::flirt::auto_generate_libc_signatures().unwrap();
    let p = sigs.into_iter().find(|s| s.name == "printf").unwrap();
    let mut w_count = 0;
    for x in &p.pattern {
        if x.is_none() { w_count += 1; }
    }
    println!("printf wildcards: {}", w_count);
}
