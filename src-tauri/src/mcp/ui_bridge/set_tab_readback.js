// Page-id read-back for POST /ui-bridge/control/page/set-tab.
//
// Embedded into the set-tab eval expression by `page.rs` via include_str!, and
// loaded verbatim by src/components/app/__tests__/set-tab-readback.test.ts —
// keep it a single self-contained function declaration with no imports.
//
// Returns:
// - pageId: the FIRST [data-page-id] in document order (the outer page wrapper).
// - activePageId: the deepest VISIBLE [data-page-id] inside that wrapper
//   (the wrapper itself included). Visible = non-zero bounding client rect, so
//   unmounted and display:none views are skipped. Greatest ancestor depth wins;
//   ties go to the last in document order.
// - pageIdChain: data-page-id values along the winner's ancestor path, outer to
//   inner, with consecutive duplicates collapsed (a page that re-publishes its
//   wrapper's id contributes one entry).
function readSetTabPageIds(doc) {
  var outer = doc.querySelector("[data-page-id]");
  var pageId = outer ? outer.getAttribute("data-page-id") : null;
  if (!outer) return { pageId: null, activePageId: null, pageIdChain: [] };

  var candidates = [outer];
  var nested = outer.querySelectorAll("[data-page-id]");
  for (var i = 0; i < nested.length; i++) candidates.push(nested[i]);

  var best = null;
  var bestDepth = -1;
  for (var j = 0; j < candidates.length; j++) {
    var cand = candidates[j];
    var rect = cand.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) continue;
    var depth = 0;
    for (var p = cand.parentElement; p; p = p.parentElement) depth++;
    if (depth >= bestDepth) {
      best = cand;
      bestDepth = depth;
    }
  }

  var chain = [];
  for (var n = best; n; n = n.parentElement) {
    if (!n.hasAttribute("data-page-id")) continue;
    var id = n.getAttribute("data-page-id");
    if (chain[0] !== id) chain.unshift(id);
  }

  return {
    pageId: pageId,
    activePageId: best ? best.getAttribute("data-page-id") : null,
    pageIdChain: chain,
  };
}
