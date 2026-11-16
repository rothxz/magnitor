//! Times one Task Manager reading (libproc for this user's processes, `ps` for the rest):
//! `cargo run --release --example sample`.
fn main() {
    for _ in 0..5 {
        let start = std::time::Instant::now();
        let sample = magnitor_data::sample::take().expect("a reading");
        let detailed = sample.procs.iter().filter(|p| p.detailed).count();
        println!("{:.1} ms: {} processes, {detailed} read with libproc", start.elapsed().as_secs_f64() * 1e3, sample.procs.len());
    }
}
