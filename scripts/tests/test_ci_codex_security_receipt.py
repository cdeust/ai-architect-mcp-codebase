"""A clean Codex security review: completed, flags null/null, verdict in the receipt.

Fixture: native summary6604865025 and receipt6624079289 on PR473 (2026-11-04),
the head 88d987b blocked as review_pending for three days with both reviews done.
"""

import json
import sys
import unittest
from unittest.mock import patch

from ci_codex_editor_contract import BOT_ID, PR, REPO, SHA, reader

SHORT = SHA[:10]
RECEIPT = (
    "<!-- codex-review-receipt:v1 -->\n\n"
    "### 🛡️ Codex Security Review · _Automatically triggered_\n\n"
    "Security review completed. No security issues were found in this pull request.\n\n"
    f"**Reviewed commit:** `{SHORT}`\n\n"
    "[View security finding report](https://chatgpt.com/codex/cloud/tasks/"
    "task_e_6aeacd45bbc48324aeb1bf76a9913eab)\n\n"
    "_Only the user who started this review can view the report in Codex._\n\n"
    "<details> <summary>ℹ️ About Codex security reviews in GitHub</summary>\n<br/>\n\n"
    "This is an experimental Codex feature. Security reviews are triggered when:\n"
    '- You comment "@codex security review"\n'
    '- A regular code review gets triggered (for example, "@codex review" or when a'
    " PR is opened), and you’re opted in so security review runs alongside code review\n\n"
    "Once complete, Codex will leave suggestions, or a comment if no findings are found.\n\n\n"
    "</details>"
)


def bot(identity, body, stamp="2026-11-04T06:17:11Z"):
    return dict(
        id=identity,
        updated_at=stamp,
        body=body,
        editor=None,
        lastEditedAt=None,
        user=dict(login="chatgpt-codex-connector[bot]", type="Bot", node_id=BOT_ID),
    )


def summary(security_flags=(None, None)):
    code = dict(commit_sha=SHA, status="completed", has_any_findings=False,
                has_unresolved_findings=False)
    security = dict(commit_sha=SHA, status="completed",
                    has_any_findings=security_flags[0],
                    has_unresolved_findings=security_flags[1])
    metadata = dict(blockingSeverityThreshold="P0", headSha=SHA, mergeGateEnabled=False,
                    pullRequestNumber=PR, repository=REPO, status="completed")
    return bot(1, (
        "<!-- codex-pull-request-review-summary -->\n"
        "<!-- codex-review-state:v1 "
        + json.dumps(dict(code_review=code, security_review=security)) + " -->\n"
        "<!-- codex-security-review:v1 " + json.dumps(metadata) + " -->\n"
        "## Codex Review Summary"
    ))


class SecurityReceiptTests(unittest.TestCase):
    def evaluate(self, rows):
        module = reader()
        runtime = sys.modules["ci_codex_review_runtime"]
        with patch.object(runtime, "_api", return_value={"sha": SHA}) as api:
            result = module.review_evidence(rows, SHA, REPO, PR)
        return result, api

    def test_no_issues_receipt_completes_a_null_security_verdict(self):
        result, api = self.evaluate([summary(), bot(2, RECEIPT, "2026-11-04T06:17:08Z")])
        self.assertEqual((result["state"], result["reason"]), ("success", "reviews_complete"))
        self.assertTrue(result["reviews_complete"])
        self.assertEqual(result["security_review"]["resolution"]["receipt_ids"], [2])
        api.assert_called_once_with(f"repos/{REPO}/commits/{SHORT}")

    def test_null_verdict_without_receipt_stays_pending(self):
        result, _ = self.evaluate([summary()])
        self.assertEqual((result["state"], result["reason"]), ("pending", "review_pending"))
        self.assertFalse(result["reviews_complete"])

    def test_receipt_for_another_head_proves_nothing(self):
        rows = [summary(), bot(2, RECEIPT)]
        module = reader()
        runtime = sys.modules["ci_codex_review_runtime"]
        with patch.object(runtime, "_api", return_value={"sha": "b" * 40}):
            result = module.review_evidence(rows, SHA, REPO, PR)
        self.assertEqual(result["state"], "pending")

    def test_unrecognized_receipt_wording_blocks_under_its_own_name(self):
        altered = RECEIPT.replace("No security issues were found", "1 issue was found")
        result, _ = self.evaluate([summary(), bot(2, altered)])
        self.assertEqual(
            (result["state"], result["reason"]),
            ("blocked", "codex_security_receipt_unrecognized"),
        )
        self.assertFalse(result["reviews_complete"])

    def test_receipt_from_a_human_is_ignored(self):
        forged = bot(2, RECEIPT)
        forged["user"] = dict(login="writer", type="User", node_id="USER_fixture")
        result, _ = self.evaluate([summary(), forged])
        self.assertEqual(result["state"], "pending")

    def test_explicit_flags_never_use_the_receipt(self):
        result, api = self.evaluate([summary((False, False)), bot(2, RECEIPT)])
        self.assertEqual(result["state"], "success")
        api.assert_not_called()


if __name__ == "__main__":
    unittest.main()
