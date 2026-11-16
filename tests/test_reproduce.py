import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('reproduce', Path(__file__).resolve().parents[1] / 'reproduce.py')
reproduce = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reproduce)


class ReproductionChecks(unittest.TestCase):
    def test_different_compiled_code_is_rejected_before_signature_attachment(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'program.exe'
            binary.write_bytes(b'changed code')
            reference = {'artifacts': {'runtime': {'unsigned_sha256': 'different'}}}
            with mock.patch.object(reproduce, 'run') as external:
                with self.assertRaisesRegex(ValueError, 'Unsigned runtime differs'):
                    reproduce.attach_signature(binary, 'runtime', reference)
                external.assert_not_called()
            self.assertEqual(binary.read_bytes(), b'changed code')

    def test_signature_attachment_cannot_replace_a_file_with_unexpected_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'program.exe'
            binary.write_bytes(b'compiled code')
            reference = {'artifacts': {'runtime': {'unsigned_sha256': reproduce.sha(binary),
                         'sha256': 'wrong final hash', 'signature': 'runtime.p7b'}},
                         'certificate_sha256': 'public certificate', 'verification_time': 1}
            def fake_attachment(arguments, capture=False):
                Path(arguments[-1]).write_bytes(b'unexpected output')
            with mock.patch.object(reproduce, 'run', side_effect=fake_attachment):
                with self.assertRaisesRegex(ValueError, 'Signed runtime differs'):
                    reproduce.attach_signature(binary, 'runtime', reference)
            self.assertEqual(binary.read_bytes(), b'compiled code')

    def test_modified_public_signature_is_rejected(self):
        actual = reproduce.REPRO
        with tempfile.TemporaryDirectory() as directory:
            import shutil
            copy = Path(directory) / 'reproducibility'
            shutil.copytree(actual, copy)
            (copy / 'signatures/runtime.p7b').write_bytes(b'not the release signature')
            with mock.patch.object(reproduce, 'REPRO', copy):
                with self.assertRaisesRegex(ValueError, 'public signature differs'):
                    reproduce.release()


if __name__ == '__main__':
    unittest.main()
