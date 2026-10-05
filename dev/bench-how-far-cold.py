#!/usr/bin/env python3
"""Artifact-cold builds with warm package/OS caches; elapsed time and total userspace instructions.
Fresh target directory per sample, incremental disabled, offline. No cache dropping.
Run without concurrent local benchmarks. Includes cargo and dependency compilation.
"""
import argparse, hashlib, json, os, pathlib, statistics, subprocess, tempfile, time
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--baseline',type=pathlib.Path,required=True)
p.add_argument('--encoder',type=pathlib.Path)
p.add_argument('--runs',type=int,default=3)
p.add_argument('--output',type=pathlib.Path,required=True)
a=p.parse_args();root=pathlib.Path(__file__).resolve().parent.parent
cases={
 'baseline-core':(a.baseline,'how-far',False,[]),
 'core':(root,'how-far',False,[]),
 'core-checked':(root,'how-far',False,['checked']),
 'core-adapters':(root,'how-far',False,['adapters']),
 'tree-minimal':(root,'how-far-along',False,[]),
 'tree-default':(root,'how-far-along',True,[]),
 'callback':(root,'how-far-along',False,['std','callback']),
 'diagnostics':(root,'how-far-really',True,[]),
}
def fingerprint(repo):
 digest=hashlib.sha256()
 names=subprocess.check_output(['git','ls-files','-z','--cached','--others','--exclude-standard','crates'],cwd=repo).split(b'\0')
 for name in sorted(set(names)):
  if name and (repo/os.fsdecode(name)).is_file():
   digest.update(name+b'\0');digest.update((repo/os.fsdecode(name)).read_bytes())
 return digest.hexdigest()
result={'rustc':subprocess.check_output(['rustc','--version'],text=True).strip(),
 'method':'fresh target per sample; CARGO_INCREMENTAL=0; offline; warm filesystem/package caches; perf includes cargo and dependency compiler processes',
 'crates_source_sha256':fingerprint(root), 'baseline_crates_source_sha256':fingerprint(a.baseline), 'cases':{}}
with tempfile.TemporaryDirectory(prefix='howfar-cold-') as tmp:
 tmp=pathlib.Path(tmp);projects={}
 for name,(repo,crate,default,features) in cases.items():
  path=tmp/name;(path/'src').mkdir(parents=True)
  (path/'src/lib.rs').write_text('#![no_std]\npub use '+crate.replace('-','_')+'::*;\n')
  (path/'Cargo.toml').write_text(f'[package]\nname="cold-probe"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[dependencies]\n{crate}={{path="{repo}/crates/{crate}",default-features={str(default).lower()},features={json.dumps(features)}}}\n')
  projects[name]=(path,[])
 if a.encoder:
  projects.update({'encoder-baseline':(a.encoder/'base',['--no-default-features']),
   'encoder-feature-off':(a.encoder/'progress',['--no-default-features']),
   'encoder-feature-on':(a.encoder/'progress',['--no-default-features','--features','progress'])})
 for name,(path,flags) in projects.items():
  result['cases'][name]={}
  for profile,cmd in [('check',['check']),('debug',['build']),('release',['build','--release'])]:
   samples=[]
   for repeat in range(a.runs):
    target=tmp/f'target-{name}-{profile}-{repeat}'
    env=dict(os.environ,CARGO_TARGET_DIR=str(target),CARGO_INCREMENTAL='0')
    perf=tmp/'perf.csv'
    argv=['cargo',*cmd,'--lib','--offline',*flags]
    start=time.perf_counter()
    run=subprocess.run(['perf','stat','-x,','-o',str(perf),'-e','instructions:u','--',*argv],cwd=path,env=env,capture_output=True,text=True)
    elapsed=time.perf_counter()-start
    if run.returncode: raise RuntimeError(run.stderr[-5000:])
    count=int(next(l for l in perf.read_text().splitlines() if 'instructions:u' in l).split(',')[0])
    samples.append({'seconds':elapsed,'instructions':count})
   med={'seconds':statistics.median(s['seconds'] for s in samples),'instructions':statistics.median(s['instructions'] for s in samples),'samples':samples}
   result['cases'][name][profile]=med
   print(name,profile,round(med['seconds'],3),round(med['instructions']/1e6,1),flush=True)
   a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(result,indent=2)+'\n')
