//! Adversarial cancellation-latency cases, one per codec/operation.
//!
//! Each case wraps a `PollMeter` and runs a deliberately expensive or
//! high-frequency-poll workload. The meter never stops the operation, so the
//! run always completes and reports the full poll traffic.

use almost_enough::PollMeter;
use enough::{Stop, Unstoppable};
use std::time::Instant;

pub struct Case {
    pub name: &'static str,
    pub codec: &'static str,
    pub blurb: &'static str,
    pub run: fn(&PollMeter<Unstoppable>) -> Result<String, String>,
}

pub fn err<E: core::fmt::Display>(e: E) -> String {
    format!("{e}")
}

/// Build the scenario registry for the enabled features.
pub fn all() -> Vec<Case> {
    let mut v: Vec<Case> = Vec::new();

    // Not a codec: measures PollMeter's own per-call overhead against the
    // bare token so instrumentation cost is a number, not a guess.
    v.push(Case {
        name: "meter-overhead-1m",
        codec: "self",
        blurb: "1M check() calls on PollMeter vs Unstoppable — direct overhead A/B",
        run: |m| {
            let n = 1_000_000u32;
            let bare = Unstoppable;
            let t0 = Instant::now();
            for _ in 0..n {
                let _ = enough::Stop::check(&bare);
            }
            let bare_t = t0.elapsed();
            let t1 = Instant::now();
            for _ in 0..n {
                let _ = m.check();
            }
            let metered_t = t1.elapsed();
            Ok(format!(
                "bare {:?} ({:.1}ns/call) metered {:?} ({:.1}ns/call) overhead {:.1}ns/call",
                bare_t,
                bare_t.as_nanos() as f64 / n as f64,
                metered_t,
                metered_t.as_nanos() as f64 / n as f64,
                (metered_t.as_nanos() as f64 - bare_t.as_nanos() as f64) / n as f64,
            ))
        },
    });

    #[cfg(feature = "zenflate")]
    {
        v.push(Case {
            name: "zenflate-effort200-16mb",
            codec: "zenflate",
            blurb: "deflate_compress at effort 200 (30+ optimal-parse iterations) on 16MB mixed data",
            run: |m| {
                let data = crate::inputs::bytes_mixed(16 << 20, 0xF1A7E);
                let mut c =
                    zenflate::compress::Compressor::new(zenflate::compress::CompressionLevel::new(
                        200,
                    ));
                let bound =
                    zenflate::compress::Compressor::deflate_compress_bound(data.len());
                let mut out = vec![0u8; bound];
                let t = Instant::now();
                let n = c
                    .deflate_compress(&data, &mut out, m)
                    .map_err(err)?;
                Ok(format!(
                    "{}B -> {}B in {:?}",
                    data.len(),
                    n,
                    t.elapsed()
                ))
            },
        });
        v.push(Case {
            name: "zenflate-effort200-small",
            codec: "zenflate",
            blurb: "effort 200 on 256KB — checks whether small inputs still see poll traffic",
            run: |m| {
                let data = crate::inputs::bytes_mixed(256 << 10, 0xCAFE);
                let mut c =
                    zenflate::compress::Compressor::new(zenflate::compress::CompressionLevel::new(
                        200,
                    ));
                let bound =
                    zenflate::compress::Compressor::deflate_compress_bound(data.len());
                let mut out = vec![0u8; bound];
                let t = Instant::now();
                let n = c
                    .deflate_compress(&data, &mut out, m)
                    .map_err(err)?;
                Ok(format!(
                    "{}B -> {}B in {:?}",
                    data.len(),
                    n,
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zenpng")]
    {
        v.push(Case {
            name: "zenpng-maniac-2048",
            codec: "zenpng",
            blurb: "encode_rgb8 Compression::Maniac on 2048x2048 (both cancel+deadline metered)",
            run: |m| {
                use rgb::FromSlice;
                let px = crate::inputs::rgb8_photo(2048, 2048, 7);
                let img = imgref::ImgRef::new(px.as_rgb(), 2048, 2048);
                let cfg = zenpng::EncodeConfig::default()
                    .with_compression(zenpng::Compression::Maniac);
                let t = Instant::now();
                let out = zenpng::encode_rgb8(img, None, &cfg, m, m).map_err(err)?;
                Ok(format!(
                    "2048x2048 -> {}B in {:?}",
                    out.len(),
                    t.elapsed()
                ))
            },
        });
        v.push(Case {
            name: "zenpng-decode-maniac-out",
            codec: "zenpng",
            blurb: "decode the maniac-compressed PNG back (decode poll pattern)",
            run: |m| {
                use rgb::FromSlice;
                // Encode unmetered first — this case isolates DECODE polling.
                let px = crate::inputs::rgb8_photo(2048, 2048, 7);
                let img = imgref::ImgRef::new(px.as_rgb(), 2048, 2048);
                let cfg = zenpng::EncodeConfig::default()
                    .with_compression(zenpng::Compression::Balanced);
                let png = zenpng::encode_rgb8(img, None, &cfg, &Unstoppable, &Unstoppable)
                    .map_err(err)?;
                let t = Instant::now();
                let out = zenpng::decode(&png, &zenpng::PngDecodeConfig::default(), m)
                    .map_err(err)?;
                Ok(format!(
                    "{}B png -> {}x{} in {:?}",
                    png.len(),
                    out.info.width,
                    out.info.height,
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zenjpeg")]
    {
        v.push(Case {
            name: "zenjpeg-encode-progressive-4k",
            codec: "zenjpeg",
            blurb: "EncodeRequest progressive encode of 3840x2160 RGBA",
            run: |m| {
                let px = crate::inputs::rgba8_photo(3840, 2160, 11);
                let cfg = zenjpeg::encoder::EncoderConfig::rgb(90)
                    .progressive(zenjpeg::encoder::ProgressiveScanMode::Smallest);
                let t = Instant::now();
                let jpeg = cfg
                    .request()
                    .stop(m)
                    .encode_bytes(&px, 3840, 2160, zenjpeg::encoder::PixelLayout::Rgba8Srgb)
                    .map_err(err)?;
                Ok(format!(
                    "3840x2160 -> {}B jpeg in {:?}",
                    jpeg.len(),
                    t.elapsed()
                ))
            },
        });
        v.push(Case {
            name: "zenjpeg-decode-progressive-4k",
            codec: "zenjpeg",
            blurb: "DecodeConfig::decode on a large progressive JPEG",
            run: |m| {
                let px = crate::inputs::rgba8_photo(3840, 2160, 11);
                let cfg = zenjpeg::encoder::EncoderConfig::rgb(90)
                    .progressive(zenjpeg::encoder::ProgressiveScanMode::Smallest);
                let jpeg = cfg
                    .request()
                    .encode_bytes(&px, 3840, 2160, zenjpeg::encoder::PixelLayout::Rgba8Srgb)
                    .map_err(err)?;
                let t = Instant::now();
                let out = zenjpeg::decoder::DecodeConfig::new()
                    .decode(&jpeg, m)
                    .map_err(err)?;
                Ok(format!(
                    "{}B jpeg -> {}x{} in {:?}",
                    jpeg.len(),
                    out.width(),
                    out.height(),
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zenwebp")]
    {
        v.push(Case {
            name: "zenwebp-lossy-m6-1024",
            codec: "zenwebp",
            blurb: "EncodeRequest::lossy method=6 (full trellis) on 1024x1024 RGBA",
            run: |m| {
                let px = crate::inputs::rgba8_photo(1024, 1024, 13);
                let mut cfg = zenwebp::LossyConfig::new();
                cfg.quality = 75.0;
                cfg.method = 6;
                let t = Instant::now();
                let webp = zenwebp::EncodeRequest::lossy(
                    &cfg,
                    &px,
                    zenwebp::PixelLayout::Rgba8,
                    1024,
                    1024,
                )
                .with_stop(m)
                .encode()
                .map_err(err)?;
                Ok(format!(
                    "1024x1024 -> {}B webp in {:?}",
                    webp.len(),
                    t.elapsed()
                ))
            },
        });
        v.push(Case {
            name: "zenwebp-lossless-2048",
            codec: "zenwebp",
            blurb: "lossless encode 2048x2048 (VP8L backward-refs heavy path)",
            run: |m| {
                let px = crate::inputs::rgba8_photo(2048, 2048, 17);
                let cfg = zenwebp::LosslessConfig::default();
                let t = Instant::now();
                let webp = zenwebp::EncodeRequest::lossless(
                    &cfg,
                    &px,
                    zenwebp::PixelLayout::Rgba8,
                    2048,
                    2048,
                )
                .with_stop(m)
                .encode()
                .map_err(err)?;
                Ok(format!(
                    "2048x2048 -> {}B lossless webp in {:?}",
                    webp.len(),
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zengif")]
    {
        v.push(Case {
            name: "zengif-encode-64f",
            codec: "zengif",
            blurb: "encode_gif of 64 animated 512x512 frames (palette quantization)",
            run: |m| {
                use rgb::FromSlice;
                let frames: Vec<zengif::FrameInput> = (0..64)
                    .map(|i| {
                        let px = crate::inputs::rgba8_photo(512, 512, 100 + i);
                        zengif::FrameInput::new(
                            512,
                            512,
                            4,
                            px.as_rgba().to_vec()
                                .into_iter()
                                .map(|p| zengif::Rgba::new(p.r, p.g, p.b, p.a))
                                .collect(),
                        )
                    })
                    .collect();
                let t = Instant::now();
                let gif = zengif::encode_gif(
                    frames,
                    512,
                    512,
                    zengif::EncoderConfig::new(),
                    zengif::Limits::default(),
                    m,
                )
                .map_err(err)?;
                Ok(format!(
                    "64 frames -> {}B gif in {:?}",
                    gif.len(),
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zenbitmaps")]
    {
        v.push(Case {
            name: "zenbitmaps-pam-8k",
            codec: "zenbitmaps",
            blurb: "encode_pam + decode roundtrip of 8K RGBA — fast path, poll-storm watch",
            run: |m| {
                let px = crate::inputs::rgba8_photo(7680, 4320, 23);
                let t = Instant::now();
                let pam = zenbitmaps::encode_pam(
                    &px,
                    7680,
                    4320,
                    zenbitmaps::PixelLayout::Rgba8,
                    m,
                )
                .map_err(err)?;
                let dec = zenbitmaps::decode(&pam, m).map_err(err)?;
                Ok(format!(
                    "{}B pam, decoded {} bytes in {:?}",
                    pam.len(),
                    dec.pixels().len(),
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zenzop")]
    {
        v.push(Case {
            name: "zenzop-squeeze-enhanced-4mb",
            codec: "zenzop",
            blurb: "enhanced zopfli, maxblocks=1 (largest DP per squeeze iter) on 4MB mixed",
            run: |m| {
                use std::io::Write;
                let data = crate::inputs::bytes_mixed(4 << 20, 0x20F1);
                let mut opts = zenzop::Options::default();
                opts.enhanced = true;
                // One block per 1MB master chunk: the squeeze loop checks
                // once per iteration, so this maximizes per-check work.
                opts.maximum_block_splits = 1;
                let t = Instant::now();
                let mut enc = zenzop::DeflateEncoder::with_stop(opts, Vec::new(), m);
                enc.write_all(&data).map_err(err)?;
                let res = enc.finish().map_err(err)?;
                let out = res.into_inner();
                Ok(format!(
                    "{}B -> {}B in {:?}",
                    data.len(),
                    out.len(),
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zenavif")]
    {
        v.push(Case {
            name: "zenavif-decode-kodim03",
            codec: "zenavif",
            blurb: "decode_with on kodim03_yuv420_8bpc test vector",
            run: |m| {
                let avif = include_bytes!(
                    "/home/lilith/work/zen/zenavif/tests/vectors/libavif/kodim03_yuv420_8bpc.avif"
                );
                let t = Instant::now();
                let out = zenavif::decode_with(avif, &zenavif::DecoderConfig::default(), m)
                    .map_err(err)?;
                let (w, h) = (out.width(), out.height());
                Ok(format!(
                    "{}B avif -> {}x{} in {:?}",
                    avif.len(),
                    w,
                    h,
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "butteraugli")]
    {
        v.push(Case {
            name: "butteraugli-2048",
            codec: "butteraugli",
            blurb: "compare_with_stop on two 2048x2048 sRGB images",
            run: |m| {
                let a = crate::inputs::rgb8_photo(2048, 2048, 31);
                let b = crate::inputs::rgb8_photo(2048, 2048, 37);
                let t = Instant::now();
                let reference = butteraugli::precompute::ButteraugliReference::new(
                    &a,
                    2048,
                    2048,
                    butteraugli::ButteraugliParams::default(),
                )
                .map_err(err)?;
                let res = reference.compare_with_stop(&b, m).map_err(err)?;
                Ok(format!("score {:.3} in {:?}", res.score, t.elapsed()))
            },
        });
    }

    #[cfg(feature = "fast-ssim2")]
    {
        v.push(Case {
            name: "fast-ssim2-2048",
            codec: "fast-ssim2",
            blurb: "compute_ssimulacra2_with_config on 2048x2048 RGB8 pair (simd)",
            run: |m| {
                let a = crate::inputs::rgb8_photo(2048, 2048, 41);
                let b = crate::inputs::rgb8_photo(2048, 2048, 43);
                let src = fast_ssim2::PixelSlice::new(
                    &a,
                    2048,
                    2048,
                    2048 * 3,
                    fast_ssim2::PixelDescriptor::RGB8_SRGB,
                )
                .map_err(err)?;
                let dst = fast_ssim2::PixelSlice::new(
                    &b,
                    2048,
                    2048,
                    2048 * 3,
                    fast_ssim2::PixelDescriptor::RGB8_SRGB,
                )
                .map_err(err)?;
                let cfg = fast_ssim2::Ssimulacra2Config::simd().with_stop(m);
                let t = Instant::now();
                let score =
                    fast_ssim2::compute_ssimulacra2_with_config(&src, &dst, &cfg).map_err(err)?;
                Ok(format!("ssim2 {:.3} in {:?}", score, t.elapsed()))
            },
        });
    }

    #[cfg(feature = "zenjxl")]
    {
        v.push(Case {
            name: "zenjxl-decode-2048",
            codec: "zenjxl",
            blurb: "decode_with_options on a generated 2048x2048 JXL (Arc<dyn Stop> meter)",
            run: |m| {
                let px = crate::inputs::rgb8_photo(2048, 2048, 61);
                // Unmetered setup: produce a real JXL to decode.
                let jxl = jxl_encoder::LossyConfig::new(2.0)
                    .encode(&px, 2048, 2048, jxl_encoder::PixelLayout::Rgb8)
                    .map_err(err)?;
                let t = Instant::now();
                let out = zenjxl::decode_with_options(
                    &jxl,
                    None,
                    &[],
                    None,
                    Some(std::sync::Arc::new(m.clone())),
                )
                .map_err(err)?;
                Ok(format!(
                    "{}B jxl -> {}x{} in {:?}",
                    jxl.len(),
                    out.info.width,
                    out.info.height,
                    t.elapsed()
                ))
            },
        });
    }

    #[cfg(feature = "zensim")]
    {
        v.push(Case {
            name: "zensim-1024",
            codec: "zensim",
            blurb: "Zensim codec_target compute on 1024x1024 RGB8 pair",
            run: |m| {
                use rgb::FromSlice;
                let a = crate::inputs::rgb8_photo(1024, 1024, 53);
                let b = crate::inputs::rgb8_photo(1024, 1024, 59);
                let src = imgref::ImgRef::new(a.as_rgb(), 1024, 1024);
                let dst = imgref::ImgRef::new(b.as_rgb(), 1024, 1024);
                let z = zensim::Zensim::new(zensim::ZensimProfile::codec_target())
                    .with_stop(m.clone());
                let t = Instant::now();
                let res = z.compute(&src, &dst).map_err(err)?;
                Ok(format!("score {:.3} in {:?}", res.score(), t.elapsed()))
            },
        });
    }

    v
}
