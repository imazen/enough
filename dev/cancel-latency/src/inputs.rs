//! Deterministic synthetic inputs — photo-ish gradients + noise, no deps.

/// Small fast PRNG (xorshift64*) so inputs are reproducible.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn byte(&mut self) -> u8 {
        (self.next() >> 32) as u8
    }
}

/// Interleaved RGB8 with smooth gradients + texture + noise — compressible
/// but not degenerate (flat input unrealistically favors codecs).
pub fn rgb8_photo(w: usize, h: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f64 / w as f64;
            let fy = y as f64 / h as f64;
            let wave = (fx * 12.0).sin() * (fy * 9.0).cos();
            let r = (fx * 255.0 + wave * 40.0 + (rng.next() % 9) as f64) as u8;
            let g = (fy * 255.0 * 0.8 + wave * 30.0 + (rng.next() % 9) as f64) as u8;
            let b = ((1.0 - fx * fy) * 200.0 + wave * 25.0 + (rng.next() % 9) as f64) as u8;
            out.extend_from_slice(&[r, g, b]);
        }
    }
    out
}

/// Interleaved RGBA8 variant of [`rgb8_photo`]; alpha varies smoothly.
pub fn rgba8_photo(w: usize, h: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f64 / w as f64;
            let fy = y as f64 / h as f64;
            let wave = (fx * 12.0).sin() * (fy * 9.0).cos();
            let r = (fx * 255.0 + wave * 40.0 + (rng.next() % 9) as f64) as u8;
            let g = (fy * 255.0 * 0.8 + wave * 30.0 + (rng.next() % 9) as f64) as u8;
            let b = ((1.0 - fx * fy) * 200.0 + wave * 25.0 + (rng.next() % 9) as f64) as u8;
            let a = (192.0 + wave * 60.0) as u8;
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

/// Byte stream mixing compressible runs and incompressible noise — adversarial
/// for match finders (matches are far apart and short).
pub fn bytes_mixed(n: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        // 1-4KB of compressible pattern, then a noise burst.
        let pat_len = 1024 + (rng.next() % 3072) as usize;
        let pat: Vec<u8> = (0..16).map(|_| rng.byte()).collect();
        for i in 0..pat_len.min(n - out.len()) {
            out.push(pat[i % pat.len()] ^ (i % 3 == 0) as u8);
        }
        let noise_len = 512 + (rng.next() % 1536) as usize;
        for _ in 0..noise_len.min(n - out.len()) {
            out.push(rng.byte());
        }
    }
    out
}
