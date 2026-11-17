// The getter of `transform` on an SVG element: an SVGAnimatedTransformList that reads the attribute. Read only. `consolidate()` gives
// the product of the list, or null for an empty list. d3 parses a transform with it. A list that does not parse is empty, as in Chrome.
(function () {
  const item = /^[\s,]*(matrix|translate|scale|rotate|skewX|skewY)\s*\(([^)]*)\)/;
  const counts = { matrix: [6], translate: [1, 2], scale: [1, 2], rotate: [1, 3], skewX: [1], skewY: [1] };
  function parse(text) {
    const m = new DOMMatrix();
    let n = 0;
    let rest = String(text);
    while (rest.replace(/[\s,]+/, "") !== "") {
      const r = item.exec(rest);
      if (!r) return [null, 0];
      const a = r[2].split(/[\s,]+/).filter(Boolean).map(Number);
      if (!counts[r[1]].includes(a.length) || a.some((x) => !Number.isFinite(x))) return [null, 0];
      switch (r[1]) {
        case "matrix": m.multiplySelf(new DOMMatrix(a)); break;
        case "translate": m.translateSelf(a[0], a[1] || 0); break;
        case "scale": m.scaleSelf(a[0], a.length > 1 ? a[1] : a[0]); break;
        case "rotate": m.translateSelf(a[1] || 0, a[2] || 0).rotateSelf(0, 0, a[0]).translateSelf(-(a[1] || 0), -(a[2] || 0)); break;
        case "skewX": m.skewXSelf(a[0]); break;
        case "skewY": m.skewYSelf(a[0]); break;
      }
      n++;
      rest = rest.slice(r[0].length);
    }
    return [m, n];
  }
  return function transform() {
    const [m, n] = parse(this.getAttribute("transform") ?? "");
    const list = { numberOfItems: n, consolidate: () => (n ? { type: 1, angle: 0, matrix: m } : null) };
    return { baseVal: list, animVal: list };
  };
})()
