//! Times one Task Manager reading (libproc for this user's processes, `ps` for the rest) and the
//! CPU die sensors alone, and a reading that leaves `ps` out: `cargo run --release --example sample`.
fn main() {
    for _ in 0..5 {
        let start = std::time::Instant::now();
        let sample = magnitor_data::sample::take(true).expect("a reading");
        let detailed = sample.procs.iter().filter(|p| p.detailed).count();
        println!("{:.1} ms: {} processes, {detailed} read with libproc", start.elapsed().as_secs_f64() * 1e3, sample.procs.len());
    }
    // The two readings of three that leave `ps` out (and find the temperature still fresh).
    for _ in 0..5 {
        let start = std::time::Instant::now();
        let sample = magnitor_data::sample::take(false).expect("a reading");
        println!("{:.1} ms without ps: {} processes", start.elapsed().as_secs_f64() * 1e3, sample.procs.len());
    }
    for _ in 0..3 {
        let start = std::time::Instant::now();
        let t = magnitor_data::sensors::cpu_temperature();
        println!("{:.1} ms: CPU {t:?} °C", start.elapsed().as_secs_f64() * 1e3);
    }
}
