fn main() {
    // Check how many sigs are loaded from the compile-time blob
    let sigs = mini_decompiler::default_sigs::load_libc_sigs();
    println!("Loaded {} sigs from embedded blob", sigs.len());
    
    // Try to find printf in the sigs
    let printf_sig = sigs.iter().find(|s| s.name == "printf");
    match printf_sig {
        Some(s) => println!("Found printf sig, len={}", s.pattern.len()),
        None => println!("printf NOT FOUND in sigs"),
    }
    
    // Load the stripped binary and try matching manually
    let bytes = std::fs::read("bin/testing_static_stripped").unwrap();
    use object::{Object, ObjectSection};
    let obj = object::File::parse(&*bytes).unwrap();
    let text = obj.section_by_name(".text").unwrap();
    let text_data = text.data().unwrap();
    let text_addr = text.address();
    
    // Scan every sub_ function boundary and try matching
    let mut matched = 0usize;
    let step = 16usize;
    for off in (0..text_data.len()).step_by(step) {
        let buf = &text_data[off..];
        for sig in &sigs {
            if sig.matches(buf) {
                matched += 1;
                if matched <= 5 {
                    println!("  match: {} at text+0x{:x}", sig.name, off);
                }
                break;
            }
        }
    }
    println!("Total pattern matches scanning text: {}", matched);
}
