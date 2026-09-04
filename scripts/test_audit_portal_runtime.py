from __future__ import annotations

import contextlib
import hashlib
import io
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path

import audit_portal_runtime as audit
from fmn_python.schema_provenance import SCHEMA_PROVENANCE_VERSION


def status_text(*rows: str) -> str:
    return "\n".join(("[status]", "# symbol\tstatus\tevidence\ttests\tnotes", *rows, ""))


def row(symbol: str, status: str = "same") -> str:
    return f"{symbol}\t{status}\tevidence\ttests\tnote"


def mark_placeholder(value, *, kind: str, symbol: str):
    value._fmn_schema_placeholder = True
    value._fmn_schema_placeholder_kind = kind
    value._fmn_schema_placeholder_symbol = symbol
    return value


class RuntimeAuditTests(unittest.TestCase):
    def module(self, name: str) -> types.ModuleType:
        module = types.ModuleType(name)
        self.addCleanup(sys.modules.pop, name, None)
        sys.modules[name] = module
        return module

    def test_reviewed_real_function_and_method_pass(self) -> None:
        module = self.module("fake_portal.real")

        def function():
            return 1

        class Widget:
            def method(self):
                return 2

        module.function = function
        module.Widget = Widget
        rows = audit.parse_status_rows(
            status_text(
                row("fake_portal.real:function", "same"),
                row("fake_portal.real:Widget.method", "improved"),
            )
        )
        report = audit.audit_rows(rows)
        self.assertTrue(report["ok"])
        self.assertEqual(report["counts"]["reviewed_implemented"], 2)
        self.assertEqual(report["counts"]["contradictions"], 0)

    def test_schema_placeholders_fail_reviewed_claims(self) -> None:
        module = self.module("fake_portal.placeholder")

        def unavailable():
            raise NotImplementedError

        mark_placeholder(
            unavailable,
            kind="function",
            symbol="fake_portal.placeholder:function",
        )
        module.function = unavailable
        rows = audit.parse_status_rows(status_text(row("fake_portal.placeholder:function")))
        report = audit.audit_rows(rows)
        self.assertFalse(report["ok"])
        self.assertEqual(report["counts"]["runtime_placeholders"], 1)
        contradiction = report["contradictions"][0]
        self.assertEqual(contradiction["code"], "reviewed-symbol-is-placeholder")
        self.assertIn("kind=function", contradiction["detail"])
        self.assertIn("declared=fake_portal.placeholder:function", contradiction["detail"])

    def test_wrapped_placeholder_descriptors_fail_reviewed_claims(self) -> None:
        module = self.module("fake_portal.wrapped_placeholder")

        def static_unavailable():
            raise NotImplementedError

        def class_unavailable(cls):
            raise NotImplementedError(cls)

        mark_placeholder(
            static_unavailable,
            kind="method",
            symbol="fake_portal.wrapped_placeholder:Widget.static_unavailable",
        )
        mark_placeholder(
            class_unavailable,
            kind="method",
            symbol="fake_portal.wrapped_placeholder:Widget.class_unavailable",
        )

        class Widget:
            pass

        Widget.static_unavailable = staticmethod(static_unavailable)
        Widget.class_unavailable = classmethod(class_unavailable)
        module.Widget = Widget
        report = audit.audit_rows(
            audit.parse_status_rows(
                status_text(
                    row(
                        "fake_portal.wrapped_placeholder:Widget.static_unavailable",
                        "same",
                    ),
                    row(
                        "fake_portal.wrapped_placeholder:Widget.class_unavailable",
                        "improved",
                    ),
                )
            )
        )
        self.assertFalse(report["ok"])
        self.assertEqual(report["counts"]["runtime_placeholders"], 2)
        self.assertEqual(
            {item["symbol"] for item in report["contradictions"]},
            {
                "fake_portal.wrapped_placeholder:Widget.static_unavailable",
                "fake_portal.wrapped_placeholder:Widget.class_unavailable",
            },
        )
        self.assertEqual(
            {item["code"] for item in report["contradictions"]},
            {"reviewed-symbol-is-placeholder"},
        )

    def test_synthesized_class_identity_fails_reviewed_claim(self) -> None:
        module = self.module("fake_portal.placeholder_class")

        class Generated:
            pass

        mark_placeholder(
            Generated,
            kind="class",
            symbol="fake_portal.placeholder_class:Generated",
        )
        module.Generated = Generated
        report = audit.audit_rows(
            audit.parse_status_rows(
                status_text(row("fake_portal.placeholder_class:Generated"))
            )
        )
        self.assertFalse(report["ok"])
        self.assertEqual(report["counts"]["runtime_placeholders"], 1)
        self.assertEqual(
            report["contradictions"][0]["code"],
            "reviewed-symbol-is-placeholder",
        )

    def test_placeholder_owner_invalidates_inherited_lifecycle_member(self) -> None:
        module = self.module("fake_portal.placeholder_owner")

        class Generated:
            def setup(self):
                return None

        mark_placeholder(
            Generated,
            kind="class",
            symbol="fake_portal.placeholder_owner:Generated",
        )
        module.Generated = Generated
        report = audit.audit_rows(
            audit.parse_status_rows(
                status_text(row("fake_portal.placeholder_owner:Generated.setup"))
            )
        )
        self.assertFalse(report["ok"])
        self.assertEqual(report["counts"]["runtime_placeholders"], 1)
        contradiction = report["contradictions"][0]
        self.assertEqual(
            contradiction["code"],
            "reviewed-symbol-has-placeholder-owner",
        )
        self.assertIn("fake_portal.placeholder_owner:Generated", contradiction["detail"])

    def test_placeholder_marker_is_direct_not_inherited(self) -> None:
        module = self.module("fake_portal.direct_marker")

        class Generated:
            pass

        mark_placeholder(
            Generated,
            kind="class",
            symbol="fake_portal.direct_marker:Generated",
        )

        class Authored(Generated):
            value = 7

        module.Authored = Authored
        report = audit.audit_rows(
            audit.parse_status_rows(
                status_text(row("fake_portal.direct_marker:Authored.value"))
            )
        )
        self.assertTrue(report["ok"])
        self.assertEqual(report["counts"]["runtime_placeholders"], 0)

    def test_tiered_and_excluded_placeholders_do_not_claim_implementation(self) -> None:
        module = self.module("fake_portal.boundary")

        def unavailable():
            raise NotImplementedError

        mark_placeholder(
            unavailable,
            kind="function",
            symbol="fake_portal.boundary:tiered",
        )
        module.tiered = unavailable
        module.excluded = unavailable
        rows = audit.parse_status_rows(
            status_text(
                row("fake_portal.boundary:tiered", "tiered"),
                row("fake_portal.boundary:excluded", "excluded"),
            )
        )
        report = audit.audit_rows(rows)
        self.assertTrue(report["ok"])
        self.assertEqual(report["counts"]["reviewed_implemented"], 0)
        self.assertEqual(report["counts"]["runtime_placeholders"], 0)

    def test_missing_reviewed_symbol_and_module_fail_closed(self) -> None:
        self.module("fakeWÜÜ[›Z\ÜÚ[™×ÜÞ[X›ÛŠBˆ›ÝÜÈH]Y]œ\œÙWÜÝ]\×Ü›ÝÜÊˆÝ]\×Ý^
ˆ›ÝÊ™˜ZÙWÜÜ[›Z\ÜÚ[™×ÜÞ[X›Û››ÜHŠKˆ›ÝÊ™˜ZÙWÜÜ[››×ÜÝXÚÛ[Ù[N››ÜHŠKˆ
Bˆ
Bˆ™\ÜH]Y]˜]Y]Ü›ÝÜÊ›ÝÜÊBˆÙ[‹˜\ÜÙ\˜[ÙJ™\ÜÈ›ÚÈ—JBˆÙ[‹˜\ÜÙ\\]X[
™\ÜÈ˜ÛÝ[È—VÈ›Z\ÜÚ[™×Ü™]šY]ÙY—KŠBˆÙ[‹˜\ÜÙ\\]X[
ˆÚ][VÈ˜ÛÙH—H›Üˆ][H[ˆ™\ÜÈ˜ÛÛ˜YXÝ[ÛœÈ—_KˆÈ›Z\ÜÚ[™Ë\™]šY]ÙY\Þ[X›Û‹›[Ù[KZ[\ÜY˜Z[YŸKˆ
B‚ˆYˆ\ÝÛX[š[[X—Ü›ÝÜ×Ü™\]Z\™WÜØÚ[XWÜ›Ý™[˜[˜ÙJÙ[ŠHOˆ›Û™N‚ˆ[Ù[HH\\Ë“[Ù[U\J›X[š[[X‹˜]Y]Ùš^\™HŠBˆ[Ù[K˜[YHHBˆ›ÝÜÈH]Y]œ\œÙWÜÝ]\×Ü›ÝÜÊˆÝ]\×Ý^
›ÝÊ›X[š[[X‹˜]Y]Ùš^\™N˜[YHŠJBˆ
Bˆ[\Ü\ˆH[X™HÛ˜[YNˆ[Ù[B‚ˆZ\ÜÚ[™ÈH]Y]˜]Y]Ü›ÝÜÊ›ÝÜË[\Ü\Z[\Ü\ŠBˆÙ[‹˜\ÜÙ\˜[ÙJZ\ÜÚ[™ÖÈ›ÚÈ—JBˆÙ[‹˜\ÜÙ\\]X[
ˆZ\ÜÚ[™ÖÈ˜ÛÛ˜YXÝ[ÛœÈ—VÌVÈ˜ÛÙH—Kˆœ[[YK\ØÚ[XK\›Ý™[˜[˜ÙK[Z\ÜÚ[™È‹ˆ
B‚ˆ[Ù[K—Ù›[—ÜØÚ[XWÜ›Ý™[˜[˜ÙWÝ™\œÚ[ÛˆHÐÒPWÔ“Õ‘SSÑWÕ‘T”ÒSÓˆ
ÈBˆZ\ÛX]ÚYH]Y]˜]Y]Ü›ÝÜÊ›ÝÜË[\Ü\Z[\Ü\ŠBˆÙ[‹˜\ÜÙ\˜[ÙJZ\ÛX]ÚYÈ›ÚÈ—JBˆÙ[‹˜\ÜÙ\\]X[
ˆZ\ÛX]ÚYÈ˜ÛÛ˜YXÝ[ÛœÈ—VÌVÈ˜ÛÙH—Kˆœ[[YK\ØÚ[XK\›Ý™[˜[˜ÙK]™\œÚ[Û‹[Z\ÛX]Ú‹ˆ
B‚ˆ[Ù[K—Ù›[—ÜØÚ[XWÜ›Ý™[˜[˜ÙWÝ™\œÚ[ÛˆHÐÒSPWÔ“Õ‘SSÑWÕ‘T”ÒSÓ‚ˆ˜[YH]Y]˜]Y]Ü›ÝÜÊ›ÝÜË[\Ü\Z[\Ü\ŠBˆÙ[‹˜\ÜÙ\YJ˜[YÈ›ÚÈ—JB‚ˆYˆ\ÝÙ[˜[ZX×Ü™\ÛÛ][Û—Ù˜Z[\™WÚ\×ØWØ›Ý[™YØÛÛ˜YXÝ[ÛŠÙ[ŠHOˆ›Û™N‚ˆ[Ù[HHÙ[‹›[Ù[J™˜ZÙWÜÜ[™[˜[ZX×Ù˜Z[\™HŠBˆY\ÜØYÙHHžˆ
ˆLÌ‚ˆYˆ[˜[ZXÊ˜[YJN‚ˆ˜Z\ÙH[[YQ\œ›ÜŠˆžÛ˜[Y_NžÛY\ÜØYÙ_HŠB‚ˆ[Ù[K—×ÙÙ]]—×ÈH[˜[ZXÂˆ™\ÜH]Y]˜]Y]Ü›ÝÜÊˆ]Y]œ\œÙWÜÝ]\×Ü›ÝÜÊˆÝ]\×Ý^
›ÝÊ™˜ZÙWÜÜ[™[˜[ZX×Ù˜Z[\™N™^Ù\ÈŠJBˆ
Bˆ
BˆÙ[‹˜\ÜÙ\˜[ÙJ™\ÜÈ›ÚÈ—JBˆÙ[‹˜\ÜÙ\\]X[
™\ÜÈ˜ÛÝ[È—VÈ›Z\ÜÚ[™×Ü™]šY]ÙY—KJBˆÛÛ˜YXÝ[ÛˆH™\ÜÈ˜ÛÛ˜YXÝ[ÛœÈ—VÌBˆÙ[‹˜\ÜÙ\\]X[
ˆÛÛ˜YXÝ[Û–È˜ÛÙH—Kˆœ™]šY]ÙY\Þ[X›Û\™\ÛÛ][Û‹Y˜Z[Y‹ˆ
BˆÙ[‹˜\ÜÙ\\ÜÊ[ŠÛÛ˜YXÝ[Û–È™]Z[—JKÌÌ
BˆÙ[‹˜\ÜÙ\YJÛÛ˜YXÝ[Û–È™]Z[—K™[™ÝÚ]
¸ )ˆŠJB‚ˆYˆ\ÝÜÝ]X×Ü™\ÛÛ][Û—ÙÙ\×Û›ÝÙ^XÝ]WÙ\ØÜš\ÜœÊÙ[ŠHOˆ›Û™N‚ˆ[Ù[HHÙ[‹›[Ù[J™˜ZÙWÜÜ[™\ØÜš\ÜˆŠB‚ˆÛ\ÜÈ^ÜÚ]™Q\ØÜš\ÜŽ‚ˆYˆ×ÙÙ]×ÊÙ[‹[œÝ[˜ÙKÝÛ™\ŠN‚ˆ˜Z\ÙH[[YQ\œ›ÜŠ™\ØÜš\Üˆ^XÝ]YŠB‚ˆÛ\ÜÈÚYÙ]‚ˆY[X™\ˆH^ÜÚ]™Q\ØÜš\ÜŠ
B‚ˆ[Ù[K•ÚYÙ]HÚYÙ]ˆ™\ÜH]Y]˜]Y]Ü›ÝÜÊˆ]Y]œ\œÙWÜÝ]\×Ü›ÝÜÊˆÝ]\×Ý^
›ÝÊ™˜ZÙWÜÜ[™\ØÜš\ÜŽ•ÚYÙ]›Y[X™\ˆŠJBˆ
Bˆ
BˆÙ[‹˜\ÜÙ\YJ™\ÜÈ›ÚÈ—JB‚ˆYˆ\ÝØÛÛ˜YXÝ[Ûœ×Ø\™WÜÛÜYØžWÜÞ[X›Û
Ù[ŠHOˆ›Û™N‚ˆ[Ù[HHÙ[‹›[Ù[J™˜ZÙWÜÜ[œÛÜŠBˆ›ÝÜÈH]Y]œ\œÙWÜÝ]\×Ü›ÝÜÊˆÝ]\×Ý^
ˆ›ÝÊ™˜ZÙWÜÜ[œÛÜž™]HŠKˆ›ÝÊ™˜ZÙWÜÜ[œÛÜ˜[HŠKˆ
Bˆ
Bˆ™\ÜH]Y]˜]Y]Ü›ÝÜÊ›ÝÜÊBˆÙ[‹˜\ÜÙ\\]X[
ˆÚ][VÈœÞ[X›Û—H›Üˆ][H[ˆ™\ÜÈ˜ÛÛ˜YXÝ[ÛœÈ—WKˆÈ™˜ZÙWÜÜ[œÛÜ˜[H‹™˜ZÙWÜÜ[œÛÜž™]H—Kˆ
B‚ˆYˆ\ÝÜ\œÙ\—Ü™Z™XÝ×Ù\XØ]WÝ[šÛ›ÝÛ—Ø[™ÛX[›Ü›YYÜ›ÝÜÊÙ[ŠHOˆ›Û™N‚ˆØ\Ù\ÈH
ˆÝ]\×Ý^
›ÝÊ™˜ZÙNžŠK›ÝÊ™˜ZÙNžŠJKˆÝ]\×Ý^
›ÝÊ™˜ZÙNž‹œ™][™ŠJKˆ–ÜÝ]\×W™˜ZÙNžØ[YWÛ×™]×ˆ‹ˆ–ÜÝ]\×W›Z\ÜÚ[™ËXÛÛÛ—Ø[YWW—ˆ‹ˆ–ÜÝ]\×Wˆ‹ˆ
Bˆ›Üˆ^[ˆØ\Ù\Î‚ˆÚ]Ù[‹œÝX•\Ý
^]^
N‚ˆÚ]Ù[‹˜\ÜÙ\˜Z\Ù\Ê]Y]]Y]\œ›ÜŠN‚ˆ]Y]œ\œÙWÜÝ]\×Ü›ÝÜÊ^
B‚ˆYˆ\ÝÚœÛÛ—Ù[™[ÜWØš[™×Ù^XÝÛÝ™\›^WØž]\ÊÙ[ŠHOˆ›Û™N‚ˆ[Ù[HHÙ[‹›[Ù[J™˜ZÙWÜÜ[šœÛÛˆŠBˆ[Ù[KžHBˆ^HÝ]\×Ý^
›ÝÊ™˜ZÙWÜÜ[šœÛÛŽžŠJBˆ™\ÜH]Y]˜]Y]ÛÝ™\›^J^
Bˆ^[ØYHœÛÛ‹›ØYÊ]Y]œ™[™\—ÚœÛÛŠ™\Ü
JBˆÙ[‹˜\ÜÙ\\]X[
^[ØYÈœØÚ[XH—K™›[‹œÜ[œ[[YKX]Y]ŠBˆÙ[‹˜\ÜÙ\\]X[
^[ØYÈ™\œÚ[Ûˆ—KJBˆÙ[‹˜\ÜÙ\YJ^[ØYÈ›ÚÈ—JBˆÙ[‹˜\ÜÙ\\]X[
ˆ^[ØYÈ›Ý™\›^WÜÚLMˆ—Kˆ\ÚX‹œÚLMŠ^™[˜ÛÙJ]‹NŠJKš^YÙ\Ý

Kˆ
B‚ˆYˆ\ÝÛXZ[—ØÚXÚ×Ü™]\›œ×ÛÛ™WÝÚ]Ý]ÚY[™×Ü™\Ü
Ù[ŠHOˆ›Û™N‚ˆ[Ù[HHÙ[‹›[Ù[J™˜ZÙWÜÜ[˜ÛHŠB‚ˆYˆXÙZÛ\Š
N‚ˆ˜Z\ÙH›Ý[\[Y[Y\œ›Ü‚‚ˆX\š×ÜXÙZÛ\ŠˆXÙZÛ\‹ˆÚ[™H™[˜Ý[Ûˆ‹ˆÞ[X›ÛH™˜ZÙWÜÜ[˜ÛNœXÙZÛ\ˆ‹ˆ
Bˆ[Ù[KœXÙZÛ\ˆHXÙZÛ\‚ˆ^HÝ]\×Ý^
›ÝÊ™˜ZÙWÜÜ[˜ÛNœXÙZÛ\ˆŠJBˆÚ][\š[K•[\Ü˜\žQ\™XÝÜžJ
H\È\™XÝÜžN‚ˆ]H]
\™XÝÜžJHÈ›Ý™\›^KÝˆ‚ˆ]Üš]WÝ^
^[˜ÛÙ[™ÏH]‹NŠBˆÝÝ]H[Ë”Ýš[™ÒSÊ
BˆÝ\œˆH[Ë”Ýš[™ÒSÊ
BˆÚ]ÛÛ^X‹œ™Y\™XÝÜÝÝ]
ÝÝ]
KÛÛ^X‹œ™Y\™XÝÜÝ\œŠÝ\œŠN‚ˆÛÙHH]Y]›XZ[ŠÈ‹K[Ý™\›^H‹ÝŠ]
K‹KXÚXÚÈ—JBˆ^[ØYHœÛÛ‹›ØYÊÝÝ]™Ù]˜[YJ
JBˆÙ[‹˜\ÜÙ\\]X[
ÛÙKJBˆÙ[‹˜\ÜÙ\\]X[
Ý\œ‹™Ù]˜[YJ
KˆŠBˆÙ[‹˜\ÜÙ\˜[ÙJ^[ØYÈ›ÚÈ—JBˆÙ[‹˜\ÜÙ\\]X[
ˆ^[ØYÈ›Ý™\›^WÜÚLMˆ—Kˆ\ÚX‹œÚLMŠ^™[˜ÛÙJ]‹NŠJKš^YÙ\Ý

Kˆ
B‚‚šYˆ×Û˜[YW×ÈOH—×ÛXZ[—×ÈŽ‚ˆ[š]\Ý›XZ[Š
B