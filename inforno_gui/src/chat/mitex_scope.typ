// Typst definitions for the helpers MiTeX's output calls. MiTeX's own
// `mitex-scope` Typst package isn't shipped with the Rust crate, so every
// LaTeX command that MiTeX maps to a helper (\text, \boxed, \operatorname,
// \mathbf, pmatrix, aligned, ...) needs a definition here, or the whole
// formula fails to compile. Prepended to every formula by latex.rs.

// --- helpers ---------------------------------------------------------------
// MiTeX passes TeX arguments as math content ("red" arrives as `r e d`);
// flatten that back into a plain string.
#let mitex-str(c) = {
  if type(c) == str { c }
  else if c == none { "" }
  else if c.has("text") { mitex-str(c.text) }
  else if c.has("children") { c.children.map(mitex-str).join("") }
  else if c.has("body") { mitex-str(c.body) }
  else { "" }
}
#let mitex-color(c) = {
  let s = mitex-str(c).replace(" ", "")
  let named = (
    black: black, white: white, gray: gray, grey: gray, silver: silver,
    red: red, maroon: maroon, orange: orange, yellow: yellow, olive: olive,
    lime: lime, green: green, teal: teal, aqua: aqua, cyan: aqua,
    blue: blue, navy: navy, purple: purple, fuchsia: fuchsia, magenta: fuchsia,
    eastern: eastern, brown: rgb("#8b4513"), pink: rgb("#ff69b4"),
  )
  if s.starts-with("#") and s.len() in (4, 7) { rgb(s) }
  else { named.at(lower(s), default: none) }
}
#let mitex-length(c) = {
  let m = mitex-str(c).replace(" ", "").match(regex("^(-?[0-9]*\.?[0-9]+)(em|ex|mu|pt|mm|cm|in)$"))
  if m == none { return 0pt }
  let v = float(m.captures.at(0))
  let unit = (em: 1em, ex: 0.5em, mu: 1em / 18, pt: 1pt, mm: 1mm, cm: 1cm, "in": 1in)
  v * unit.at(m.captures.at(1))
}
// `\sqrt[3]{x}` arrives as `mitexsqrt([3], x)` with the brackets still inside.
#let mitex-unbracket(c) = {
  if c.has("children") {
    let ch = c.children
    if ch.len() >= 2 and mitex-str(ch.first()) == "[" and mitex-str(ch.last()) == "]" {
      return ch.slice(1, -1).join()
    }
  }
  c
}
#let mitex-op(name) = math.op(name, limits: true)

// --- roots, text, fonts ----------------------------------------------------
#let mitexsqrt(..args) = {
  let p = args.pos()
  if p.len() == 1 { math.sqrt(p.at(0)) } else { math.root(mitex-unbracket(p.at(0)), p.at(1)) }
}
#let textmath(it) = it
#let textrm(it) = it
#let textnormal(it) = it
#let textup(it) = text(style: "normal", it)
#let textmd(it) = text(weight: "regular", it)
#let textbf(it) = text(weight: "bold", it)
// Text inside an equation uses the math font, which has no italic face, so
// italic text names New Computer Modern (bundled, and matches the math font).
#let textit(it) = text(font: "New Computer Modern", style: "italic", it)
#let emph(it) = textit(it)
#let textsf(it) = it
#let texttt(it) = text(font: "DejaVu Sans Mono", it)
#let mitexmathbf(it) = math.bold(math.upright(it))
#let mitexbold(..args) = math.bold(math.upright(args.pos().join()))
#let mitexitalic(..args) = math.italic(args.pos().join())
#let mitexupright(..args) = math.upright(args.pos().join())
#let mitexsans(..args) = math.sans(args.pos().join())
#let mitexmono(..args) = math.mono(args.pos().join())
#let mitexcal(..args) = math.cal(args.pos().join())
#let mitexfrak(..args) = math.frak(args.pos().join())
#let mitexdisplay(..args) = math.display(args.pos().join())
#let mitexinline(..args) = math.inline(args.pos().join())
#let mitexscript(..args) = math.script(args.pos().join())
#let mitexsscript(..args) = math.sscript(args.pos().join())
#let TeX = [TeX]
#let LaTeX = [LaTeX]
#let KaTeX = [KaTeX]

// --- color, boxes ----------------------------------------------------------
#let mitexcolor(c, ..rest) = {
  let col = mitex-color(c)
  let body = rest.pos().join()
  if col == none { body } else { text(fill: col, body) }
}
#let colortext(c, body) = mitexcolor(c, body)
// Re-enters an equation inside the box; measure by glyph bounds so tall
// content (display fractions) doesn't poke out of the frame.
#let mitex-boxed-body(it) = {
  set text(top-edge: "bounds", bottom-edge: "bounds")
  $display(it)$
}
#let boxed(it) = box(stroke: 0.5pt + white, inset: (x: 3pt, y: 3pt), baseline: 30%, mitex-boxed-body(it))
#let fbox(it) = boxed(it)
#let hbox(it) = it
#let colorbox(c, it) = {
  let col = mitex-color(c)
  box(fill: if col == none { none } else { col.transparentize(60%) }, inset: (x: 3pt, y: 3pt), baseline: 30%, mitex-boxed-body(it))
}
#let raisebox(..args) = args.pos().last()

// --- operators -------------------------------------------------------------
#let operatorname(it) = math.op(math.upright(it))
#let operatornamewithlimits(it) = math.op(math.upright(it), limits: true)
#let argmax = mitex-op("arg max")
#let argmin = mitex-op("arg min")
#let plim = mitex-op("plim")
#let injlim = mitex-op("inj lim")
#let projlim = mitex-op("proj lim")
#let arctg = math.op("arctg")
#let arcctg = math.op("arcctg")
#let cotg = math.op("cotg")
#let cosec = math.op("cosec")
#let sh = math.op("sh")
#let ch = math.op("ch")
#let th = math.op("th")
#let cth = math.op("cth")
#let pmod(it) = $quad (mod it)$
#let pod(it) = $quad (it)$
#let smallint = math.inline(sym.integral)

// --- spacing, phantoms -----------------------------------------------------
#let hspace(len) = h(mitex-length(len))
#let vspace(len) = none
#let enspace = h(0.5em)
#let negthinspace = h(-1em / 6)
#let negmedspace = h(-2em / 9)
#let negthickspace = h(-5em / 18)
#let negthinmedspace = h(-2em / 9)
#let phantom(it) = hide(it)
#let hphantom(it) = box(height: 0pt, hide($it$))
#let vphantom(it) = box(width: 0pt, hide($it$))
#let mathclap(it) = box(width: 0pt, align(center, $it$))

// --- math classes ----------------------------------------------------------
#let mathbin(it) = math.class("binary", it)
#let mathrel(it) = math.class("relation", it)
#let mathop(it) = math.class("large", it)
#let mathord(it) = math.class("normal", it)
#let mathopen(it) = math.class("opening", it)
#let mathclose(it) = math.class("closing", it)
#let mathpunct(it) = math.class("punctuation", it)
#let mathinner(it) = it

// --- delimiters ------------------------------------------------------------
#let big(it) = math.lr(it, size: 1.2em)
#let Big(it) = math.lr(it, size: 1.8em)
#let bigg(it) = math.lr(it, size: 2.4em)
#let Bigg(it) = math.lr(it, size: 3em)
#let middle(it) = math.mid(it)
#let bra(it) = $lr(angle.l it |)$
#let ket(it) = $lr(| it angle.r)$
#let braket(it) = $lr(angle.l it angle.r)$
#let Bra = bra
#let Ket = ket
#let Braket = braket
// `set` is a Typst keyword, so latex.rs renames MiTeX's `set(` to this.
#let mitexset(it) = $lr({ it })$
#let Set = mitexset

// --- fractions, stacking ---------------------------------------------------
#let dfrac(a, b) = math.display(math.frac(a, b))
#let tfrac(a, b) = math.inline(math.frac(a, b))
#let cfrac(a, b) = math.display(math.frac(a, b))
#let dbinom(a, b) = math.display(math.binom(a, b))
#let tbinom(a, b) = math.inline(math.binom(a, b))
#let atop(a, b) = math.mat(delim: none, (a,), (b,))
#let brack(a, b) = math.mat(delim: "[", (a,), (b,))
#let overset(top, base) = math.attach(math.limits(base), t: top)
#let underset(bot, base) = math.attach(math.limits(base), b: bot)
#let stackrel = overset
// `\substack{a \\ b}`: split on the line breaks and stack the rows, so the
// breaks don't escape into (and split) the surrounding equation.
#let substack(it) = {
  let rows = ((),)
  for c in it.at("children", default: (it,)) {
    if c.func() == linebreak { rows.push(()) } else { rows.last().push(c) }
  }
  math.mat(delim: none, row-gap: 0.2em, ..rows.map(r => (r.join(),)))
}

// --- braces, accents, cancel -----------------------------------------------
#let mitexoverbrace(it) = math.limits(math.overbrace(it))
#let mitexunderbrace(it) = math.limits(math.underbrace(it))
#let mitexoverbracket(it) = math.limits(math.overbracket(it))
#let mitexunderbracket(it) = math.limits(math.underbracket(it))
#let overleftrightarrow(it) = math.accent(it, sym.arrow.l.r)
#let overrightharpoon(it) = math.accent(it, sym.harpoon.rt)
#let overleftharpoon(it) = math.accent(it, sym.harpoon.lt)
#let overgroup(it) = math.overparen(it)
#let undergroup(it) = math.underparen(it)
#let overlinesegment(it) = math.overline(it)
#let underbar(it) = math.underline(it)
#let mathring(it) = math.accent(it, "˚")
#let bcancel(it) = math.cancel(it, inverted: true)
#let xcancel(it) = math.cancel(it, cross: true)
#let sout(it) = math.cancel(it, angle: 90deg)

// --- extensible arrows -----------------------------------------------------
#let mitex-xarrow(s, ..args) = {
  let p = args.pos()
  if p.len() >= 2 { math.attach(math.stretch(s), t: p.last(), b: p.first()) }
  else { math.attach(math.stretch(s), t: p.at(0, default: none)) }
}
#let xrightarrow(..a) = mitex-xarrow(sym.arrow.r, ..a)
#let xleftarrow(..a) = mitex-xarrow(sym.arrow.l, ..a)
#let xRightarrow(..a) = mitex-xarrow(sym.arrow.r.double, ..a)
#let xLeftarrow(..a) = mitex-xarrow(sym.arrow.l.double, ..a)
#let xleftrightarrow(..a) = mitex-xarrow(sym.arrow.l.r, ..a)
#let xLeftrightarrow(..a) = mitex-xarrow(sym.arrow.l.r.double, ..a)
#let xmapsto(..a) = mitex-xarrow(sym.arrow.r.bar, ..a)
#let xlongequal(..a) = mitex-xarrow(sym.eq, ..a)
#let xhookrightarrow(..a) = mitex-xarrow(sym.arrow.r.hook, ..a)
#let xhookleftarrow(..a) = mitex-xarrow(sym.arrow.l.hook, ..a)
#let xtwoheadrightarrow(..a) = mitex-xarrow(sym.arrow.r.twohead, ..a)
#let xtwoheadleftarrow(..a) = mitex-xarrow(sym.arrow.l.twohead, ..a)
#let xrightharpoonup(..a) = mitex-xarrow(sym.harpoon.rt, ..a)
#let xrightharpoondown(..a) = mitex-xarrow(sym.harpoon.rb, ..a)
#let xleftharpoonup(..a) = mitex-xarrow(sym.harpoon.lt, ..a)
#let xleftharpoondown(..a) = mitex-xarrow(sym.harpoon.lb, ..a)
#let xrightleftharpoons(..a) = mitex-xarrow(sym.harpoons.rtlb, ..a)
#let xleftrightharpoons(..a) = mitex-xarrow(sym.harpoons.ltrb, ..a)
#let xtofrom(..a) = mitex-xarrow(sym.arrows.rl, ..a)

// --- environments ----------------------------------------------------------
#let matrix(..args) = math.mat(delim: none, ..args)
#let pmatrix(..args) = math.mat(delim: "(", ..args)
#let bmatrix(..args) = math.mat(delim: "[", ..args)
#let Bmatrix(..args) = math.mat(delim: "{", ..args)
#let vmatrix(..args) = math.mat(delim: "|", ..args)
#let Vmatrix(..args) = math.mat(delim: "‖", ..args)
#let smallmatrix(..args) = math.inline(math.mat(delim: none, ..args))
#let mitexarray(arg0: none, ..args) = math.mat(delim: none, ..args)
#let rcases(..args) = math.cases(reverse: true, ..args)
#let aligned(it) = it
#let alignedat(arg0: none, it) = it
#let mitexlabel(..args) = none
// `\ref`/`\eqref`: there's nothing to point at inside a single formula, so
// show LaTeX's own marker for an unresolved reference instead of failing.
#let mitexref(..args) = [??]

// --- symbols ---------------------------------------------------------------
#let varGamma = $italic(Gamma)$
#let varDelta = $italic(Delta)$
#let varTheta = $italic(Theta)$
#let varLambda = $italic(Lambda)$
#let varXi = $italic(Xi)$
#let varPi = $italic(Pi)$
#let varSigma = $italic(Sigma)$
#let varUpsilon = $italic(Upsilon)$
#let varPhi = $italic(Phi)$
#let varPsi = $italic(Psi)$
#let varOmega = $italic(Omega)$
#let thickapprox = sym.approx
#let thicksim = sym.tilde.op
// `\mho` is emitted as `ohm.inv`, which Typst 0.14 no longer has.
#let ohm = (inv: "℧")