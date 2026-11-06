use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("model directory"));
    if !onicode_voice::installed(&dir) {
        onicode_voice::download(&dir, &AtomicBool::new(false), |_| {})?;
    }
    let start = Instant::now();
    let mut model = onicode_voice::Recognizer::load(&dir)?;
    println!("load_ms={}", start.elapsed().as_millis());
    for file in args {
        let mut wav = hound::WavReader::open(&file)?;
        let spec = wav.spec();
        anyhow::ensure!(
            spec.channels == 1 && spec.bits_per_sample == 16,
            "mono PCM16 fixture required"
        );
        let samples: Vec<f32> = wav
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.)
            .collect();
        let start = Instant::now();
        let text = model.transcribe(samples, spec.sample_rate)?;
        println!(
            "fixture={file} inference_ms={} transcript={text:?}",
            start.elapsed().as_millis()
        );
    }
    Ok(())
}
