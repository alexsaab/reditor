#!/usr/bin/env python3
"""Disposable local FTP, explicit/implicit FTPS and SFTP servers for Rust integration tests.
Requires: pip install pyftpdlib paramiko pyopenssl (test dependencies only).
"""
import datetime
import ipaddress
import json
import logging
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time


def ftp_server(kind, root, cert, key):
    from pyftpdlib.authorizers import DummyAuthorizer
    from pyftpdlib.handlers import FTPHandler, TLS_FTPHandler
    from pyftpdlib.servers import FTPServer
    logging.disable(logging.CRITICAL)
    authorizer = DummyAuthorizer()
    authorizer.add_user('demo', 'test-password', str(root), perm='elradfmwMT')
    if kind == 'implicit':
        class Handler(TLS_FTPHandler):
            def handle(self):
                self.secure_connection(self.ssl_context)
                super().handle()
    else:
        Handler = type('Handler', (TLS_FTPHandler if kind == 'ftps' else FTPHandler,), {})
    Handler.authorizer = authorizer
    if kind != 'ftp':
        Handler.certfile = str(cert)
        Handler.keyfile = str(key)
        Handler.tls_control_required = True
        Handler.tls_data_required = True
    server = FTPServer(('127.0.0.1', 0), Handler)
    print(server.socket.getsockname()[1], flush=True)
    server.serve_forever(timeout=0.05)


def sftp_server(root, key_path):
    import paramiko
    logging.disable(logging.CRITICAL)
    host_key = paramiko.RSAKey.from_private_key_file(str(key_path))

    class Auth(paramiko.ServerInterface):
        def check_auth_password(self, username, password):
            return paramiko.AUTH_SUCCESSFUL if (username, password) == ('demo', 'test-password') else paramiko.AUTH_FAILED
        def check_auth_publickey(self, username, key):
            return paramiko.AUTH_SUCCESSFUL if username == 'demo' and key == host_key else paramiko.AUTH_FAILED
        def get_allowed_auths(self, username):
            return 'password,publickey'
        def check_channel_request(self, kind, chanid):
            return paramiko.OPEN_SUCCEEDED if kind == 'session' else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    class Handle(paramiko.SFTPHandle):
        def stat(self):
            return paramiko.SFTPAttributes.from_stat(os.fstat((self.readfile or self.writefile).fileno()))
        def chattr(self, attr):
            paramiko.SFTPServer.set_file_attr(self.filename, attr)
            return paramiko.SFTP_OK

    class Files(paramiko.SFTPServerInterface):
        def path(self, path):
            target = (root / path.lstrip('/')).resolve()
            if not target.is_relative_to(root):
                raise PermissionError('outside fixture')
            return target
        def list_folder(self, path):
            try:
                result = []
                for item in self.path(path).iterdir():
                    attr = paramiko.SFTPAttributes.from_stat(item.stat())
                    attr.filename = item.name
                    result.append(attr)
                return result
            except OSError as error:
                return paramiko.SFTPServer.convert_errno(error.errno)
        def stat(self, path):
            try:
                return paramiko.SFTPAttributes.from_stat(self.path(path).stat())
            except OSError as error:
                return paramiko.SFTPServer.convert_errno(error.errno)
        lstat = stat
        def open(self, path, flags, attr):
            try:
                filename = self.path(path)
                fd = os.open(filename, flags, attr.st_mode or 0o600)
                mode = 'r+b' if flags & os.O_RDWR else 'wb' if flags & os.O_WRONLY else 'rb'
                stream = os.fdopen(fd, mode)
                handle = Handle(flags)
                handle.filename = filename
                if flags & (os.O_WRONLY | os.O_RDWR):
                    handle.writefile = stream
                if not flags & os.O_WRONLY:
                    handle.readfile = stream
                return handle
            except OSError as error:
                return paramiko.SFTPServer.convert_errno(error.errno)
        def chattr(self, path, attr):
            try:
                paramiko.SFTPServer.set_file_attr(str(self.path(path)), attr)
                return paramiko.SFTP_OK
            except OSError as error:
                return paramiko.SFTPServer.convert_errno(error.errno)
        def mkdir(self, path, attr):
            try:
                self.path(path).mkdir(mode=attr.st_mode or 0o755)
                return paramiko.SFTP_OK
            except OSError as error:
                return paramiko.SFTPServer.convert_errno(error.errno)
        def remove(self, path):
            try:
                self.path(path).unlink()
                return paramiko.SFTP_OK
            except OSError as error:
                return paramiko.SFTPServer.convert_errno(error.errno)
        def rename(self, old, new):
            try:
                os.replace(self.path(old), self.path(new))
                return paramiko.SFTP_OK
            except OSError as error:
                return paramiko.SFTPServer.convert_errno(error.errno)
        posix_rename = rename

    listener = socket.socket()
    listener.bind(('127.0.0.1', 0))
    listener.listen(32)
    print(listener.getsockname()[1], flush=True)
    def connection(client):
        transport = paramiko.Transport(client)
        transport.add_server_key(host_key)
        transport.set_subsystem_handler('sftp', paramiko.SFTPServer, Files)
        try:
            transport.start_server(server=Auth())
            while transport.is_active():
                time.sleep(0.02)
        except (EOFError, OSError, paramiko.SSHException):
            pass
        finally:
            transport.close()
    while True:
        client, _ = listener.accept()
        threading.Thread(target=connection, args=(client,), daemon=True).start()


def main():
    if len(sys.argv) > 1 and sys.argv[1] == '--server':
        kind, root, cert, key = sys.argv[2:]
        if kind == 'sftp':
            sftp_server(Path(root), Path(key))
        else:
            ftp_server(kind, Path(root), Path(cert), Path(key))
        return
    import paramiko
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID
    processes = []
    with tempfile.TemporaryDirectory(prefix='reditor-remote-') as directory:
        root = Path(directory).resolve()
        (root / '.reditor').mkdir()
        (root / '.reditor/config.toml').write_text('rust_analyzer=""\n')
        tls_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'localhost')])
        now = datetime.datetime.now(datetime.timezone.utc)
        certificate = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
            .public_key(tls_key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(minutes=1)).not_valid_after(now + datetime.timedelta(days=1))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName('localhost'), x509.IPAddress(ipaddress.ip_address('127.0.0.1'))]), critical=False)
            .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True).sign(tls_key, hashes.SHA256()))
        cert = root / 'server.pem'
        cert.write_bytes(certificate.public_bytes(serialization.Encoding.PEM))
        key = root / 'server-key.pem'
        key.write_bytes(tls_key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.TraditionalOpenSSL, serialization.NoEncryption()))
        host_key = paramiko.RSAKey.generate(2048)
        ssh_key = root / 'ssh-key'
        host_key.write_private_key_file(str(ssh_key))
        config = []
        try:
            for kind in ['ftp', 'ftps', 'implicit', 'sftp']:
                files = root / kind
                files.mkdir()
                (files / 'sample.php').write_text("<?php echo 'Привет 🦀';\n")
                server = subprocess.Popen([sys.executable, __file__, '--server', kind, str(files), str(cert), str(ssh_key if kind == 'sftp' else key)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                processes.append(server)
                line = server.stdout.readline().strip()
                if not line:
                    raise RuntimeError(server.stderr.read())
                port = int(line)
                config.append(f'[connections.{kind}]\nprotocol={json.dumps("ftps" if kind == "implicit" else kind)}\nhost="127.0.0.1"\nport={port}\nuser="demo"\nroot="/"\npassword_env="REDITOR_TEST_PASSWORD"\ntimeout_seconds=10\n')
                if kind == 'sftp':
                    known = root / 'known_hosts'
                    known.write_text(f'[127.0.0.1]:{port} {host_key.get_name()} {host_key.get_base64()}\n')
                    config.append(f'known_hosts={json.dumps(str(known))}\n')
                    # Also test the same server with public-key authentication.
                    config.append(f'[connections.sftp_key]\nprotocol="sftp"\nhost="127.0.0.1"\nport={port}\nuser="demo"\nroot="/"\nkey_file={json.dumps(str(ssh_key))}\nknown_hosts={json.dumps(str(known))}\ntimeout_seconds=10\n')
                    (root / 'wrong_known_hosts').write_text(f'[127.0.0.1]:{port} ssh-rsa {paramiko.RSAKey.generate(2048).get_base64()}\n')
                elif kind in ['ftps', 'implicit']:
                    config.append(f'ca_file={json.dumps(str(cert))}\nimplicit_tls={str(kind == "implicit").lower()}\n')
            (root / '.reditor/connections.toml').write_text('\n'.join(config))
            env = dict(os.environ, REDITOR_REMOTE_TEST_ROOT=str(root), REDITOR_TEST_PASSWORD='test-password', REDITOR_USER_DIR=str(root / 'user'))
            subprocess.run(['cargo', 'test', '--locked', 'remote_protocol_roundtrip', '--', '--ignored', '--nocapture'], env=env, check=True)
            print('Remote smoke passed: FTP, explicit FTPS, implicit FTPS, SFTP password/key, UTF-8 paths, conflict checks and editor save queue')
        finally:
            for server in processes:
                server.terminate()
            for server in processes:
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == '__main__':
    main()
