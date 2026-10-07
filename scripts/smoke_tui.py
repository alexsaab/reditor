#!/usr/bin/env python3
"""Exercise the real terminal event loop on macOS/Linux without touching user files."""
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/reditor').resolve()
    plugins = Path(__file__).resolve().parents[1] / 'plugins'
    with_mermaid = '--mermaid' in sys.argv[2:]
    with tempfile.TemporaryDirectory(prefix='reditor-smoke-') as workspace:
        root = Path(workspace)
        (root / 'Cargo.toml').write_text(
            '[package]\nname="reditor-smoke"\nversion="0.1.0"\nedition="2024"\n'
            '[[bin]]\nname="smoke"\npath="src/nested/main.rs"\n'
        )
        fixtures = {
            'README.md': '# Привет 🦀\n\n**Markdown**\n',
            'app.js': "const value = 'Привет';\n",
            'app.ts': "const value: string = 'Привет';\n",
            'index.html': '<h1>Привет</h1>\n',
        }
        for name, source in fixtures.items():
            (root / name).write_text(source)
        markdown = '# Preview 🦀\n\n**Bold** and *italic*\n\n```mermaid\nflowchart LR\n A[Привет] --> B[Rust]\n```\n'
        (root / 'diagrams.md').write_text(markdown)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 35, 120, 0, 0))

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        process = subprocess.Popen(
            [str(binary), workspace, '--lang', 'ru', '--plugins', str(plugins)],
            stdin=slave, stdout=slave, stderr=slave,
            env={**os.environ, 'TERM': 'xterm-256color', 'REDITOR_IMAGE_PROTOCOL': 'halfblocks'},
            preexec_fn=controlling_terminal,
        )
        os.close(slave)
        output = bytearray()

        def drain(seconds=0.15):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if select.select([master], [], [], min(0.05, max(0, end - time.monotonic())))[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError:
                        return
                    if not chunk:
                        return
                    output.extend(chunk)
                    if b'\x1b[6n' in chunk:
                        os.write(master, b'\x1b[1;1R')

        def send(keys):
            os.write(master, keys.encode('utf8') if isinstance(keys, str) else keys)
            drain()

        def mouse(code, x, y, released=False):
            send(f'\x1b[<{code};{x + 1};{y + 1}{"m" if released else "M"}')

        def click(x, y):
            mouse(0, x, y)
            mouse(0, x, y, released=True)

        def await_condition(predicate, label, timeout=10):
            end = time.monotonic() + timeout
            while not predicate():
                drain()
                if process.poll() is not None or time.monotonic() > end:
                    plain = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', output.decode('utf8', errors='replace'))
                    raise AssertionError(f'{label}\n{plain[-4000:]}')

        try:
            await_condition(lambda: 'Файлы'.encode() in output, 'Initial Russian UI')
            send('\x04')
            send('src/nested\r')
            await_condition(lambda: (root / 'src/nested').is_dir(), 'Create nested directory')
            send('\x14')
            send('src/nested/main.rs\r')
            await_condition(lambda: (root / 'src/nested/main.rs').exists(), 'Create Rust file')
            send('\x1b[200~fn main(){println!("Привет 🦀");}   \n\x1b[201~')
            send('\x12')  # rustfmt
            send('\x13')  # save, including the Lua before_save hook
            rust_file = root / 'src/nested/main.rs'
            await_condition(lambda: '    println!' in rust_file.read_text(), 'Format and save Rust')
            assert 'Привет 🦀' in rust_file.read_text()
            assert not any(line.endswith(' ') for line in rust_file.read_text().splitlines())

            send('\x1bOS')  # F4
            send('\x15notes.txt\r')  # Ctrl+U, path, Enter
            notes = root / 'notes.txt'
            await_condition(notes.exists, 'Save as TXT')
            send('\x07header\r')  # Ctrl+G, JavaScript header command
            send('\x13')
            await_condition(lambda: notes.read_text().startswith('// Edited with reditor'), 'JS plugin')
            send('\x07uppercase\r')  # Ctrl+G, TypeScript uppercase command
            send('\x13')
            await_condition(lambda: 'ПРИВЕТ' in notes.read_text(), 'TypeScript plugin')

            send('\x1bOQ')  # F2 opens language menu; language remains Russian
            config = root / '.reditor/config.toml'
            assert not config.exists(), 'Opening the menu must not change the language'
            send('\x1b[B\r')  # Select English, confirm with Enter
            await_condition(lambda: config.exists() and '"en"' in config.read_text(), 'Persist language')
            send('\x1bOQ')  # Reopen the language menu; choose German with the mouse
            click(38, 16)  # centered 50x9 dialog in the 120x35 terminal: Deutsch row
            await_condition(lambda: '"de"' in config.read_text(), 'Language selection by mouse')
            send('\x1bOQ')
            send('\x1b[A\r')  # Return to English for the remaining checks
            await_condition(lambda: '"en"' in config.read_text(), 'Restore English')
            send('\x02')  # cargo check, uses the saved Rust source
            # Ratatui emits only changed cells; the status prefix may not repeat in the stream.
            await_condition(lambda: bool(list(root.glob('target/debug/.fingerprint/*/bin-smoke.json'))), 'Background cargo check', 20)
            send('\x1b[19~')  # F8 build output
            send('\x1b')
            send('\x1bOP')  # F1 help
            send('\x1b')

            # Use the real terminal mouse protocol to open, position the cursor and save.
            send('\x1b[15~')  # F5 refresh the explorer after cargo created target/
            for name, source in fixtures.items():
                entries = sorted(root.iterdir(), key=lambda p: (not p.is_dir(), p.name))
                row = 4 + next(i for i, p in enumerate(entries) if p.name == name)
                click(9, row)
                click(9, row)  # double click opens the file
                click(37, 2)  # first text cell after the activity bar and gutter
                send('\x1b[200~EDIT\x1b[201~')
                send('\x13')
                await_condition(lambda n=name, s=source: (root / n).read_text() == 'EDIT' + s, f'Mouse editing {name}')
            # Select EDIT by dragging and replace it via bracketed paste.
            mouse(0, 37, 2)
            mouse(32, 41, 2)
            mouse(0, 41, 2, released=True)
            send('\x1b[200~DONE\x1b[201~')
            send('\x13')
            await_condition(lambda: (root / 'index.html').read_text() == 'DONE' + fixtures['index.html'], 'Mouse selection replacement')

            send('\x0f')  # Open Markdown
            send(str(root / 'diagrams.md') + '\r')
            send('\x1b[18~')  # F7 preview
            send('\x1b[200~DO NOT INSERT\x1b[201~')
            send('\x13')
            assert (root / 'diagrams.md').read_text() == markdown, 'Preview must protect source text'
            if with_mermaid:
                marker = len(output)
                send('\x1b[20~')  # F9: actual Mermaid vectors and readable terminal text
                await_condition(lambda: 'Привет'.encode() in output[marker:] and b'Rust' in output[marker:], 'Readable Mermaid labels inside the terminal', 35)
                send('g')  # Optional image mode still works
                await_condition(lambda: any(g.encode() in output[marker:] for g in ['▀', '▄']), 'Mermaid image mode', 5)
                send('g')  # Return to text mode
                send('+')  # Zoom
                mouse(65, 60, 15)  # Pan by wheel
                send('e')  # Return to the Mermaid source offset
                send('\x1b[200~%% edited from the diagram view\n\x1b[201~')
                send('\x13')
                assert '%% edited from the diagram view\nflowchart LR' in (root / 'diagrams.md').read_text()
            else:
                send('\x1b[18~')  # Return to source

            send('\x11')  # Ctrl+Q
            await_condition(lambda: process.poll() is not None, 'Clean exit')
            assert process.returncode == 0
            assert b'\x1b[?2004l' in output, 'Bracketed paste must be disabled on exit'
            assert b'\x1b[?1049l' in output, 'Alternate screen must be restored'
            assert b'\x1b[?1006h' in output, 'SGR mouse reporting must be enabled'
            assert b'\x1b[?1006l' in output, 'Mouse reporting must be disabled on exit'
            print('TUI smoke passed: files, directories, UTF-8, plugins, mouse, languages, Markdown preview, terminal restore' + (', Mermaid rendering/zoom/source editing' if with_mermaid else ''))
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            os.close(master)


if __name__ == '__main__':
    main()
