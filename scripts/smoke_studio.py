#!/usr/bin/env python3
"""Exercise the workbench and bottom PTY terminal in a disposable Rust project."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/reditor').resolve()
    with tempfile.TemporaryDirectory(prefix='reditor-studio-') as directory:
        root = Path(directory)
        (root / 'src').mkdir()
        (root / 'Cargo.toml').write_text('[package]\nname="studio-smoke"\nversion="0.1.0"\nedition="2024"\n')
        (root / 'src/main.rs').write_text('fn main() { let mut s=String::new(); std::io::stdin().read_line(&mut s).unwrap(); let result=format!("RUN:{}:{}",std::env::args().nth(1).unwrap_or_default(),s.trim()); println!("{}", result); std::fs::write("run-result.txt", result).unwrap(); }\n#[test] fn smoke() { assert_eq!(2+2,4); std::fs::write("test-ok.txt", "passed").unwrap(); }\n')
        (root / 'note.txt').write_text('replace-me\n')
        (root / '.reditor').mkdir()
        # LSP has its own real-server integration test; keep this input test deterministic.
        (root / '.reditor/config.toml').write_text('language="ru"\nrust_analyzer=""\n')
        subprocess.run(['git', 'init', '-q', str(root)], check=True)
        subprocess.run(['git', '-C', str(root), 'config', 'user.name', 'Reditor Test'], check=True)
        subprocess.run(['git', '-C', str(root), 'config', 'user.email', 'test@example.invalid'], check=True)
        (root / '.gitignore').write_text('target\n.reditor\n')
        subprocess.run(['git', '-C', str(root), 'add', '.'], check=True)
        subprocess.run(['git', '-C', str(root), 'commit', '-qm', 'initial'], check=True)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 140, 0, 0))

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        env = dict(os.environ, TERM='xterm-256color', REDITOR_IMAGE_PROTOCOL='halfblocks')
        process = subprocess.Popen([str(binary), str(root / 'note.txt'), '--workspace', str(root), '--lang', 'ru'], stdin=slave, stdout=slave, stderr=slave, cwd=root, env=env, preexec_fn=controlling_terminal)
        os.close(slave)
        output = bytearray()

        def pump(seconds=0.2):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.04)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    output.extend(data)

        def send(text, wait=0.25):
            os.write(master, text.encode())
            pump(wait)

        def until(predicate, name, timeout=25):
            deadline = time.monotonic() + timeout
            while not predicate() and time.monotonic() < deadline:
                pump(0.1)
            if not predicate():
                import re
                plain = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', output.decode(errors='replace'))
                raise AssertionError(f'{name}: {plain[-1800:]}')

        def command(name):
            send('\x07')  # Ctrl+G is distinct in ordinary terminals
            send(name + '\r')
            print(f'Checked command: {name}', flush=True)

        try:
            pump(0.6)
            command('cargo.build')
            until(lambda: (root / 'target/debug/studio-smoke').exists(), 'Rust build')
            send('\x1b[21~')  # F10: back to editor
            command('cargo.args')
            send('"Привет мир"\r')
            command('cargo.run')
            send('\x1b[200~console\n\x1b[201~')
            until(lambda: (root / 'run-result.txt').exists(), 'Interactive Rust run')
            assert (root / 'run-result.txt').read_text() == 'RUN:Привет мир:console'
            send('\x1b[21~')
            command('cargo.test')
            until(lambda: (root / 'test-ok.txt').exists(), 'Rust tests')
            send('\x1b[21~')
            command('cargo.clippy')
            pump(1.5)
            send('\x1b[21~')
            command('replace.file')
            send('replace-me\r')
            send('заменено\r')
            assert (root / 'note.txt').read_text() == 'replace-me\n', 'Review must precede changes'
            send('\r')  # apply review to buffer
            send('\x13')
            assert (root / 'note.txt').read_text() == 'заменено\n'
            command('git.stage')
            pump(0.5)
            send('note.txt\r')
            pump(0.5)
            send('\x1b')
            staged = subprocess.check_output(['git', '-C', str(root), 'diff', '--cached', '--name-only']).decode()
            assert 'note.txt' in staged
            command('git.commit')
            send('editor commit\r')
            until(lambda: subprocess.check_output(['git', '-C', str(root), 'log', '-1', '--pretty=%s']).strip() == b'editor commit', 'Commit through editor')
            send('\x1b[21~')
            command('files')
            pump(0.5)
            send('note.txt\r')
            send('\x1b[200~RECOVERY\x1b[201~')
            until(lambda: (root / '.reditor/session.json').exists() and any('RECOVERY' in (tab.get('text') or '') for tab in json.loads((root / '.reditor/session.json').read_text())['tabs']), 'Recovery snapshot', 5)
            process.kill()
            process.wait(timeout=5)
            os.close(master)
            master, new_slave = pty.openpty()
            fcntl.ioctl(new_slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 140, 0, 0))
            process = subprocess.Popen([str(binary), '--workspace', str(root), '--lang', 'ru'], stdin=new_slave, stdout=new_slave, stderr=new_slave, cwd=root, env=env, preexec_fn=controlling_terminal)
            os.close(new_slave)
            pump(0.6)
            send('\x13')
            until(lambda: 'RECOVERY' in (root / 'note.txt').read_text(), 'Restore unsaved buffer after restart')
            send('\x11')
            until(lambda: process.poll() is not None, 'Clean exit', 5)
            assert process.returncode == 0
            print('Studio smoke passed: palette, build, interactive run, tests, Clippy, reviewed replacement, Git stage/commit, quick open, crash backup and restart recovery')
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            os.close(master)


if __name__ == '__main__':
    main()
