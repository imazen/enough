#!/usr/bin/env python3
"""Materialize a pinned real PNG encoder adoption outside its working checkout.

Usage: python3 dev/adopt-zenpng.py /path/to/zenpng /tmp/adoption
Creates base and progress snapshots; never modifies the encoder repository.
The progress feature compiles out the dependency and every instrumentation site.
"""
import pathlib, subprocess, sys
repo, destination = map(pathlib.Path, sys.argv[1:])
root = pathlib.Path(__file__).resolve().parent.parent
revision = '27393eef995aea329cfbc1df5cac5e3f7346b4cb'
archive = subprocess.check_output(['git', 'archive', revision], cwd=repo)
for label in ('base','progress'):
    target = destination / label
    target.mkdir(parents=True, exist_ok=True)
    subprocess.run(['tar','-x','-C',str(target)], input=archive, check=True)
    (target/'Cargo.lock').write_bytes((repo/'Cargo.lock').read_bytes())
    with (target/'Cargo.toml').open('a') as f:
        f.write(f'\nenough = {{ path = "{root}/crates/enough" }}\n') # existing patch.crates-io table
p = destination/'progress'
manifest = (p/'Cargo.toml').read_text().replace('[dependencies]', f'[dependencies]\nhow-far = {{ path = "{root}/crates/how-far", optional = true }}').replace('[features]', '[features]\nprogress = ["dep:how-far"]')
(p/'Cargo.toml').write_text(manifest)
# Copyable options let a stage substitute its pulse for deep cancellation checks.
path=p/'src/encoder/mod.rs';s=path.read_text().replace("pub(crate) struct CompressOptions<'a> {", "#[derive(Clone, Copy)]\npub(crate) struct CompressOptions<'a> {\n    #[cfg(feature = \"progress\")]\n    pub progress: Option<&'a dyn how_far::Pulse>,")
path.write_text(s)
# Initialize the optional field at all existing construction sites.
for path in (p/'src').rglob('*.rs'):
    s=path.read_text()
    import re
    s=re.sub(r'(CompressOptions\s*\{\s*\n)(?!\s*#\[derive)', r'\1            #[cfg(feature = "progress")]\n            progress: None,\n',s)
    path.write_text(s)
path=p/'src/encode.rs';s=path.read_text()
start=s.index('pub(crate) fn encode_raw('); body=s.index('    let effort =',start)
signature=s[start:body]
args='bytes, width, height, color_type, bit_depth, metadata, config, cancel, deadline'
wrapper=signature+f'    encode_raw_impl({args}, #[cfg(feature = "progress")] None)\n}}\n\n'
implsig=signature.replace('fn encode_raw(', 'fn encode_raw_impl(').replace('    deadline: &dyn Stop,','    deadline: &dyn Stop,\n    #[cfg(feature = "progress")] progress: Option<&dyn how_far::Pulse>,')
s=s[:start]+wrapper+implsig+s[body:]
start=s.index('fn encode_raw_impl('); end=s.index('/// Encode RGB8 pixels to PNG, returning',start)
chunk=s[start:end].replace('let opts = config.compress_options(cancel, deadline, None);','let opts = config.compress_options(cancel, deadline, None);\n            #[cfg(feature = "progress")]\n            let opts = crate::encoder::CompressOptions { progress, ..opts };')
s=s[:start]+chunk+s[end:]
s+='''
/// Encode RGB8 with library-owned weighted compression stages.
#[cfg(feature = "progress")]
pub fn encode_rgb8_with_pulse(
    img: ImgRef<Rgb<u8>>, metadata: Option<&Metadata>, config: &EncodeConfig,
    pulse: &dyn how_far::Pulse, deadline: &dyn Stop,
) -> crate::error::Result<Vec<u8>> {
    pulse.check().map_err(|reason| at!(crate::error::PngError::Stopped(reason)))?;
    let width = img.width() as u32;
    let height = img.height() as u32;
    let (buf, _, _) = img.to_contiguous_buf();
    let bytes = bytemuck::cast_slice(buf.as_ref());
    encode_raw_impl(bytes, width, height, ColorType::Rgb, BitDepth::Eight,
        metadata, config, pulse, deadline, Some(pulse))
}
'''
path.write_text(s)
path=p/'src/encoder/compress.rs';s=path.read_text()
# Only the existing compression algorithm is used. No duplicated codec loops.
start=s.index('pub(crate) fn compress_filtered(');end=s.index('/// Phase 0',start)
chunk=s[start:end]
needle='    let mut state = CompressState::new(filtered_size);'
chunk=chunk.replace(needle,'''    #[cfg(feature = "progress")]
    let mut stages = how_far::Stages::new(opts.progress.unwrap_or(&how_far::NoPulse), &[
        how_far::PhaseSpec::new("screen", 10, how_far::Total::Unknown).units("verified strategies"),
        how_far::PhaseSpec::new("refine", 60, how_far::Total::Unknown),
        how_far::PhaseSpec::new("bruteforce", 20, how_far::Total::Unknown),
        how_far::PhaseSpec::new("recompress", 10, how_far::Total::Unknown),
    ]).map_err(progress_plan_error)?;
'''+needle)
# Wrap each existing phase call; keep the feature-off call unchanged.
for number,name in [(1,'screen'),(2,'refine'),(3,'bruteforce'),(4,'recompress')]:
    fn=f'run_phase{number}_{name}('
    pos=chunk.index(fn); depth=1;i=pos+len(fn)
    while depth:
        depth += (chunk[i]=='(')-(chunk[i]==')');i+=1
    call=chunk[pos:i]
    replacement='''{
        #[cfg(not(feature = "progress"))]
        { ORIGINAL }
        #[cfg(feature = "progress")]
        {
            stages.run_classified(|e: &whereat::At<PngError>| matches!(e.error(), PngError::Stopped(_)), |stage| {
                stage.check().map_err(|r| at!(PngError::Stopped(r)))?;
                let cancel: &dyn enough::Stop = if opts.progress.is_some() { stage } else { opts.cancel };
                let opts = super::CompressOptions { cancel, progress: opts.progress.map(|_| stage), ..opts };
                let value = ORIGINAL?;
                stage.check().map_err(|r| at!(PngError::Stopped(r)))?;
                Ok(value)
            }).map_err(progress_run_error)
        }
    }'''.replace('ORIGINAL',call)
    chunk=chunk[:pos]+replacement+chunk[i:]
needle='    if params.screen_is_final || opts.deadline.should_stop() {'
chunk=chunk.replace(needle,needle+'''
        #[cfg(feature = "progress")]
        {
            for _ in 0..3 { stages.skip().map_err(progress_plan_error)?; }
            stages.finish().map_err(progress_plan_error)?;
        }
''')
chunk=chunk.replace('    // Same invariant as the early-return above:', '    #[cfg(feature = "progress")]\n    stages.finish().map_err(progress_plan_error)?;\n\n    // Same invariant as the early-return above:')
s=s[:start]+chunk+s[end:]
# Count verified strategy results at their existing success boundary (serial and parallel).
s=s.replace('screen_results.push((compressed_len, filtered_data));','screen_results.push((compressed_len, filtered_data));\n        #[cfg(feature = "progress")]\n        if let Some(p) = opts.progress { p.advance(1); }').replace('screen_results.push((compressed_len, state.filtered.clone()));','screen_results.push((compressed_len, state.filtered.clone()));\n            #[cfg(feature = "progress")]\n            if let Some(p) = opts.progress { p.advance(1); }')
s+='''
#[cfg(feature = "progress")]
fn progress_plan_error(error: how_far::PlanError) -> whereat::At<PngError> {
    at!(PngError::Internal(zencodec::InternalKind::Bug, error.to_string()))
}
#[cfg(feature = "progress")]
fn progress_run_error(error: how_far::RunError<whereat::At<PngError>>) -> whereat::At<PngError> {
    match error {
        how_far::RunError::Work(error) => error,
        how_far::RunError::Plan(error) => progress_plan_error(error),
        _ => at!(PngError::Internal(zencodec::InternalKind::Bug, "unknown progress error".into())),
    }
}
'''
path.write_text(s)
print(f'Pinned encoder {revision}: {destination}')

path=p/'src/lib.rs'
with path.open('a') as f: f.write('\n#[cfg(feature = "progress")]\npub use encode::encode_rgb8_with_pulse;\n')

probe=destination/'probe';(probe/'src').mkdir(parents=True,exist_ok=True)
(probe/'src/main.rs').write_text((root/'dev/encoder-probe.rs').read_text())
patch=(p/'Cargo.toml').read_text().split('[patch.crates-io]',1)[1]
(probe/'Cargo.toml').write_text(f'''[package]
name="encoder-probe"
version="0.0.0"
edition="2024"
[workspace]
[features]
progress=["zenpng/progress","dep:how-far-along"]
[dependencies]
zenpng={{path="../progress",default-features=false}}
enough={{path="{root}/crates/enough"}}
how-far-along={{path="{root}/crates/how-far-along",features=["callback"],optional=true}}
imgref="1.12"
rgb="0.8"
[patch.crates-io]
{patch}
''')
