// Rode com: node --test src/markdown.test.ts   (Node >= 22.6 remove os tipos sozinho)
import assert from "node:assert/strict";
import { test } from "node:test";
import { escapeHtml, formatBytes, formatEta, renderMarkdown } from "./markdown.ts";

test("escapa HTML e impede script/atributos", () => {
  const h = renderMarkdown('<script>alert(1)</script> <img src=x onerror="a()">');
  assert.ok(!h.includes("<script"));
  assert.ok(!h.includes("<img"));
  assert.ok(h.includes("&lt;script&gt;"));
  assert.equal(escapeHtml(`"'&<>`), "&quot;&#39;&amp;&lt;&gt;");
});

test("só links http/https viram <a>", () => {
  assert.ok(renderMarkdown("[ok](https://x.com/a?b=1&c=2)").includes('href="https://x.com/a?b=1&amp;c=2"'));
  assert.ok(!renderMarkdown("[mau](javascript:alert(1))").includes("<a"));
  assert.ok(!renderMarkdown("[mau](file:///c:/x)").includes("<a"));
});

test("link não consegue escapar do atributo href", () => {
  const h = renderMarkdown('[x](https://a.com/"onmouseover="alert(1))');
  // A tag tem exatamente href + data-ext; as aspas do payload ficam escapadas (&quot;) dentro do valor.
  assert.match(h, /^<p><a href="[^"]*" data-ext="1">x<\/a>/);
  assert.ok(h.includes("&quot;onmouseover=&quot;"));
});

test("negrito, itálico, código, títulos e listas", () => {
  assert.equal(renderMarkdown("**Evento** sexta *20h*"), "<p><strong>Evento</strong> sexta <em>20h</em></p>");
  assert.equal(renderMarkdown("`a**b**c`"), "<p><code>a**b**c</code></p>");
  assert.equal(renderMarkdown("# Aviso"), "<h3>Aviso</h3>");
  assert.equal(renderMarkdown("- um\n- dois\n\ntexto"), "<ul><li>um</li><li>dois</li></ul><p>texto</p>");
  assert.equal(renderMarkdown("linha1\nlinha2"), "<p>linha1<br>linha2</p>");
  assert.equal(renderMarkdown(""), "");
});

test("formatadores", () => {
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(5 * 1024 * 1024), "5.0 MB");
  assert.equal(formatEta(45), "45s");
  assert.equal(formatEta(125), "2min 5s");
  assert.equal(formatEta(3700), "1h 1min");
});
