#!/usr/bin/env python3
"""Build and compare pristine and progress-adopted real encoder entry points."""
import argparse,json,os,pathlib,statistics,subprocess,tempfile
p=argparse.ArgumentParser();p.add_argument('directory',type=pathlib.Path);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
root=pathlib.Path(__file__).resolve().parent.parent;probe=a.directory/'probe'
base=a.directory/'baseline-probe';(base/'src').mkdir(parents=True,exist_ok=True)
(base/'src/main.rs').write_text((root/'dev/encoder-probe.rs').read_text())
patch=(a.directory/'base/Cargo.toml').read_text().split('[patch.crates-io]',1)[1]
(base/'Cargo.toml').write_text(f'''[package]
name="encoder-probe"
version="0.0.0"
edition="2024"
[workspace]
[features]
progress=[]
[dependencies]
zenpng={{path="../base",default-features=false}}
enough={{path="{root}/crates/enough"}}
imgref="1.12"
rgb="0.8"
[patch.crates-io]
{patch}
''')
for path,flags in [(base,[]),(probe,['--features','progress'])]:
 subprocess.run(['cargo','build','--offline','--release',*flags],cwd=path,check=True)
rows=[]
for size,iterations in [(64,100),(512,20)]:
 for label,path,mode in [('baseline',base,'old'),('legacy-enabled',probe,'old'),('nopulse',probe,'nopulse'),('tree',probe,'tree'),('callback',probe,'callback')]:
  samples=[]
  for _ in range(3):
   with tempfile.NamedTemporaryFile() as f:
    run=subprocess.run(['perf','stat','-x,','-o',f.name,'-e','instructions:u','--',str(path/'target/release/encoder-probe'),mode,str(iterations),str(size)],capture_output=True,text=True,check=True)
    data=json.loads(run.stdout)
    data['instructions']=int(next(l for l in pathlib.Path(f.name).read_text().splitlines() if 'instructions:u' in l).split(',')[0]);samples.append(data)
  row={'mode':label,'size':size,'iterations':iterations,'median_instructions':statistics.median(s['instructions'] for s in samples),'median_ns':statistics.median(s['elapsed_ns'] for s in samples),'samples':samples}
  rows.append(row);print(label,size,row['median_instructions'],row['median_ns'],flush=True)
 a.output.write_text(json.dumps({'method':'perf process instructions, including one reference encode; reported elapsed excludes reference encode; all iterations assert byte parity','rows':rows},indent=2)+'\n')
