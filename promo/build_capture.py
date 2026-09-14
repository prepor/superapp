#!/usr/bin/env python3
"""Build a private capture binary; never modify the shared Cargo checkout."""
import difflib
import os
from pathlib import Path
import shutil
import subprocess
from capture import ROOT, OUT


def main():
    cargo=OUT/'cargo-home'
    src=Path.home()/'.cargo'
    (cargo/'git/checkouts').mkdir(parents=True,exist_ok=True)
    for name in ('registry','git/db'):
        path=cargo/name
        if not path.exists():path.symlink_to(src/name,target_is_directory=True)
    for path in (src/'git/checkouts').iterdir():
        target=cargo/'git/checkouts'/path.name
        if not target.exists():
            if path.name=='makepad-66e0a7dae1831997':shutil.copytree(path,target)
            else:target.symlink_to(path,target_is_directory=True)
    relative=Path('git/checkouts/makepad-66e0a7dae1831997/9523aea/platform/src/os/apple/metal.rs')
    original=(src/relative).read_text()
    marker='''                        // Metal readback for BGRA8 textures returns BGRA bytes. Convert to RGBA'''
    insertion='''                        let mut remaining_requests = sf.request_ids;
                        if std::env::var_os("MAKEPAD_CAPTURE_BMP").is_some() {
                            let mut file_requests = Vec::new();
                            remaining_requests.retain(|id| {
                                if *id >= (1u64 << 63) { file_requests.push(*id); false }
                                else { true }
                            });
                            if !file_requests.is_empty() {
                                // Capture-only path: a top-down 32-bit BMP from the actual
                                // Metal pixels. Skip PNG compression on the GPU completion
                                // thread; all studio/HTTP/pixel-probe requests stay unchanged.
                                let bytes = (sf.width * sf.height * 4) as u32;
                                let mut bmp = Vec::with_capacity(bytes as usize + 54);
                                bmp.extend_from_slice(b"BM");
                                bmp.extend_from_slice(&(bytes + 54).to_le_bytes());
                                bmp.extend_from_slice(&0u32.to_le_bytes());
                                bmp.extend_from_slice(&54u32.to_le_bytes());
                                bmp.extend_from_slice(&40u32.to_le_bytes());
                                bmp.extend_from_slice(&(sf.width as i32).to_le_bytes());
                                bmp.extend_from_slice(&(-(sf.height as i32)).to_le_bytes());
                                bmp.extend_from_slice(&1u16.to_le_bytes());
                                bmp.extend_from_slice(&32u16.to_le_bytes());
                                bmp.extend_from_slice(&0u32.to_le_bytes());
                                bmp.extend_from_slice(&bytes.to_le_bytes());
                                bmp.extend_from_slice(&[0u8;16]);
                                bmp.extend_from_slice(&bgra);
                                Cx::send_studio_screenshot_response(file_requests,
                                    sf.width as _, sf.height as _, bmp);
                            }
                        }
'''
    assert original.count(marker)==1
    changed=original.replace(marker,insertion+marker).replace(
        'let mut request_ids = sf.request_ids;', 'let mut request_ids = remaining_requests;')
    (cargo/relative).write_text(changed)
    (ROOT/'promo/makepad-capture.patch').write_text(''.join(difflib.unified_diff(
        original.splitlines(True),changed.splitlines(True),
        fromfile='a/platform/src/os/apple/metal.rs',tofile='b/platform/src/os/apple/metal.rs')))
    env=dict(os.environ,CARGO_HOME=str(cargo))
    subprocess.run(['mise','exec','--','cargo','build','--release','-p','superapp','--no-default-features'],
                   cwd=ROOT,env=env,check=True)
    shutil.copy2(ROOT/'target/release/superapp',OUT/'bin/superapp-capture')
    print('Private raw-frame capture binary ready',flush=True)


if __name__=='__main__':main()
