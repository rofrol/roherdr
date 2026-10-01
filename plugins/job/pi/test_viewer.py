"""Offline tests: only selected records reach the terminal, never execution."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("viewer", Path(__file__).with_name("viewer.py"))
viewer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(viewer)


class ViewerTests(unittest.TestCase):
    def test_selected_call_only_and_sanitised_output(self):
        entries = [
            {"message": {"role": "assistant", "content": [
                {"type": "toolCall", "id": "other", "arguments": {"secret": "not shown"}},
                {"type": "toolCall", "id": "a", "name": "bash", "arguments": {"command": "echo test"}},
            ]}},
            {"message": {"role": "toolResult", "toolCallId": "other", "content": [{"type": "text", "text": "unrelated"}]}},
            {"message": {"role": "toolResult", "toolCallId": "a", "content": [{"type": "text", "text": "\x1b[31mresult\x1b[0m"}]}},
        ]
        with tempfile.TemporaryDirectory() as root:
            session = Path(root) / "session.jsonl"
            session.write_text("malformed\n" + "\n".join(json.dumps(entry) for entry in entries) + "\n")
            output = io.StringIO()
            self.assertEqual(viewer.follow(session, "a", output, 1), 0)
        text = output.getvalue()
        self.assertIn("echo test", text)
        self.assertIn("result", text)
        self.assertNotIn("not shown", text)
        self.assertNotIn("unrelated", text)
        self.assertNotIn("\x1b", text)

    def test_failure_and_images_preserve_transcript_reference(self):
        entry = {"message": {"role": "toolResult", "toolCallId": "a", "isError": True,
                             "content": [{"type": "image", "data": "secret-base64"}]}}
        done, text = viewer.selected_payload(entry, "a")
        self.assertTrue(done)
        self.assertIn("failed", text)
        self.assertIn("original Pi transcript", text)
        self.assertNotIn("secret-base64", text)

    def test_control_sequences_removed(self):
        self.assertEqual(viewer.safe_text("\x1b]52;c;payload\x07safe\u202e\x00"), "safe")
        self.assertIsNone(viewer.selected_payload({"message": []}, "a"))


if __name__ == "__main__":
    unittest.main()
