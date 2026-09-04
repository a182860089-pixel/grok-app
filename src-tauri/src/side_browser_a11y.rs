//! Cursor-style a11y snapshot + ref actions for the embedded side browser.
//!
//! Snapshot walks visible interactive nodes, stamps `data-grok-ref="eN"`, and
//! returns a compact YAML-like tree. Click/type/hover look up that ref.

/// Page snapshot — JSON `{ ok, snapshot, refs, href, title, readyState }`.
pub const SNAPSHOT_JS: &str = r#"(function(){
  var ATTR = 'data-grok-ref';
  try {
    document.querySelectorAll('[' + ATTR + ']').forEach(function(el){
      el.removeAttribute(ATTR);
    });
  } catch (e) {}
  function visible(el) {
    if (!el || el.nodeType !== 1) return false;
    var st;
    try { st = window.getComputedStyle(el); } catch (e) { return false; }
    if (!st || st.display === 'none' || st.visibility === 'hidden') return false;
    if (parseFloat(st.opacity || '1') === 0) return false;
    var r = el.getBoundingClientRect();
    return r.width >= 1 && r.height >= 1;
  }
  function roleOf(el) {
    var r = (el.getAttribute('role') || '').trim();
    if (r) return r;
    var tag = el.tagName.toLowerCase();
    if (tag === 'a') return 'link';
    if (tag === 'button' || tag === 'summary') return 'button';
    if (tag === 'select') return 'combobox';
    if (tag === 'textarea' || el.isContentEditable) return 'textbox';
    if (tag === 'iframe') return 'iframe';
    if (tag === 'img') return 'image';
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (tag === 'input') {
      var t = (el.type || 'text').toLowerCase();
      if (t === 'submit' || t === 'button' || t === 'reset' || t === 'file' || t === 'image') return 'button';
      if (t === 'checkbox') return 'checkbox';
      if (t === 'radio') return 'radio';
      if (t === 'range') return 'slider';
      return 'textbox';
    }
    return 'generic';
  }
  function nameOf(el) {
    var acc = (el.getAttribute('aria-label') || '').trim();
    if (acc) return acc.slice(0, 120);
    if (el.tagName === 'IMG') return (el.getAttribute('alt') || '').trim().slice(0, 120);
    if (el.tagName === 'IFRAME') {
      return (el.getAttribute('title') || el.getAttribute('name') || el.getAttribute('src') || 'iframe').trim().slice(0, 120);
    }
    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') {
      var lab = (el.getAttribute('placeholder') || el.getAttribute('name') || el.getAttribute('title') || '').trim();
      if (!lab && el.id) {
        var l = document.querySelector('label[for="' + el.id.replace(/"/g, '\\"') + '"]');
        if (l) lab = (l.innerText || '').trim();
      }
      if (lab) return lab.slice(0, 120);
    }
    var t = ((el.innerText || el.textContent || '') + '').replace(/\s+/g, ' ').trim();
    return t.slice(0, 120);
  }
  var INTERACTIVE = 'a[href], button, input, select, textarea, iframe, summary, [role="button"], [role="link"], [role="tab"], [role="menuitem"], [role="checkbox"], [role="radio"], [role="textbox"], [role="combobox"], [role="switch"], [role="slider"], [contenteditable="true"]';
  var CONTEXT = 'h1, h2, h3, h4, h5, h6, img[alt], [role="heading"]';
  var lines = [];
  lines.push('- document [url=' + JSON.stringify(location.href || '') + '] [title=' + JSON.stringify(document.title || '') + '] [ready=' + (document.readyState || '') + ']');
  var n = 0;
  var seen = [];
  function already(el) {
    for (var i = 0; i < seen.length; i++) if (seen[i] === el) return true;
    return false;
  }
  var nodes;
  try { nodes = document.querySelectorAll(INTERACTIVE + ',' + CONTEXT); }
  catch (e) { nodes = []; }
  for (var i = 0; i < nodes.length && lines.length < 420; i++) {
    var el = nodes[i];
    if (already(el) || !visible(el)) continue;
    seen.push(el);
    var interactive = false;
    try { interactive = el.matches(INTERACTIVE); } catch (e) {}
    var role = roleOf(el);
    var name = nameOf(el);
    var extra = '';
    var tag = el.tagName.toLowerCase();
    if (/^h[1-6]$/.test(tag)) extra += ' [level=' + tag.charAt(1) + ']';
    if (el.disabled) extra += ' [disabled]';
    if (tag === 'input' || tag === 'textarea') {
      var val = (el.value || '').toString().slice(0, 80);
      if (val) extra += ' [value=' + JSON.stringify(val) + ']';
    }
    var line = '- ' + role + (name ? ' ' + JSON.stringify(name) : '');
    if (interactive) {
      n += 1;
      var ref = 'e' + n;
      el.setAttribute(ATTR, ref);
      line += ' [ref=' + ref + ']';
    }
    lines.push(line + extra);
  }
  return JSON.stringify({
    ok: true,
    snapshot: lines.join('\n'),
    refs: n,
    href: location.href || '',
    title: document.title || '',
    readyState: document.readyState || ''
  });
})()"#;

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

pub fn click_js(r#ref: &str) -> String {
    let r = js_str(r#ref);
    format!(
        r#"(function(){{
  var ref = {r};
  var el = document.querySelector('[data-grok-ref=' + JSON.stringify(ref) + ']');
  if (!el) return JSON.stringify({{ok:false, error:'ref not found: ' + ref + '. Call browser_snapshot first.'}});
  try {{ el.scrollIntoView({{block:'center', inline:'nearest'}}); }} catch (e) {{}}
  if (String(el.tagName || '').toUpperCase() === 'IFRAME') {{
    var frameBox = el.getBoundingClientRect();
    try {{ el.focus(); }} catch (e) {{}}
    return JSON.stringify({{ok:true, ref:ref, tag:'IFRAME', native:true, left:frameBox.left, top:frameBox.top, width:frameBox.width, height:frameBox.height}});
  }}
  try {{ el.focus(); }} catch (e) {{}}
  var box = el.getBoundingClientRect();
  var x = box.left + Math.max(1, box.width / 2);
  var y = box.top + Math.max(1, box.height / 2);
  var opts = {{bubbles:true, cancelable:true, view:window, clientX:x, clientY:y}};
  ['pointerover','mouseover','pointerdown','mousedown','pointerup','mouseup','click'].forEach(function(type){{
    try {{
      if (type.indexOf('mouse') === 0 || type === 'click') el.dispatchEvent(new MouseEvent(type, opts));
      else el.dispatchEvent(new PointerEvent(type, opts));
    }} catch (e) {{
      try {{ el.dispatchEvent(new MouseEvent(type === 'click' ? 'click' : 'mousedown', opts)); }} catch (e2) {{}}
    }}
  }});
  try {{ if (typeof el.click === 'function') el.click(); }} catch (e) {{}}
  return JSON.stringify({{ok:true, ref:ref, tag: el.tagName}});
}})()"#
    )
}

pub fn focus_frame_js(selector: Option<&str>) -> String {
    let selector = selector.map(js_str).unwrap_or_else(|| "null".into());
    format!(
        r#"(function(){{
  var selector = {selector};
  var el = null;
  try {{
    if (selector) {{
      el = document.querySelector(selector);
    }} else {{
      var frames = Array.prototype.slice.call(document.querySelectorAll('iframe'));
      el = frames.find(function(x){{
        var r = x.getBoundingClientRect();
        var s = getComputedStyle(x);
        return s.display !== 'none' && s.visibility !== 'hidden' && r.width >= 2 && r.height >= 2;
      }}) || null;
    }}
  }} catch (e) {{
    return JSON.stringify({{ok:false, error:String(e)}});
  }}
  if (!el || String(el.tagName || '').toUpperCase() !== 'IFRAME') {{
    return JSON.stringify({{ok:false, error:'iframe not found'}});
  }}
  try {{ el.scrollIntoView({{block:'center', inline:'nearest'}}); }} catch (e) {{}}
  try {{ el.focus(); }} catch (e) {{}}
  var r = el.getBoundingClientRect();
  return JSON.stringify({{
    ok:true,
    tag:'IFRAME',
    title:el.getAttribute('title') || '',
    name:el.getAttribute('name') || '',
    src:el.getAttribute('src') || '',
    left:r.left,
    top:r.top,
    width:r.width,
    height:r.height
  }});
}})()"#
    )
}

pub fn type_js(r#ref: &str, text: &str, submit: bool) -> String {
    let r = js_str(r#ref);
    let t = js_str(text);
    format!(
        r#"(function(){{
  var ref = {r};
  var text = {t};
  var submit = {submit};
  var el = document.querySelector('[data-grok-ref=' + JSON.stringify(ref) + ']');
  if (!el) return JSON.stringify({{ok:false, error:'ref not found: ' + ref + '. Call browser_snapshot first.'}});
  try {{ el.scrollIntoView({{block:'center', inline:'nearest'}}); }} catch (e) {{}}
  if (String(el.tagName || '').toUpperCase() === 'IFRAME') {{
    try {{ el.focus(); }} catch (e) {{}}
    return JSON.stringify({{ok:true, ref:ref, native:true, submit:submit, textLength:text.length}});
  }}
  try {{ el.focus(); }} catch (e) {{}}
  function setVal(node, value) {{
    if (node.isContentEditable) {{
      node.textContent = value;
      return;
    }}
    var proto = node.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    var desc = Object.getOwnPropertyDescriptor(proto, 'value');
    if (desc && desc.set) desc.set.call(node, value);
    else node.value = value;
  }}
  setVal(el, text);
  try {{ el.dispatchEvent(new Event('input', {{bubbles:true}})); }} catch (e) {{}}
  try {{ el.dispatchEvent(new Event('change', {{bubbles:true}})); }} catch (e) {{}}
  if (submit) {{
    var form = el.form || el.closest('form');
    if (form) {{
      try {{ if (form.requestSubmit) form.requestSubmit(); else form.submit(); }} catch (e) {{}}
    }} else {{
      try {{ el.dispatchEvent(new KeyboardEvent('keydown', {{key:'Enter', code:'Enter', keyCode:13, bubbles:true}})); }} catch (e) {{}}
    }}
  }}
  return JSON.stringify({{ok:true, ref:ref, submit:submit}});
}})()"#
    )
}

pub fn hover_js(r#ref: &str) -> String {
    let r = js_str(r#ref);
    format!(
        r#"(function(){{
  var ref = {r};
  var el = document.querySelector('[data-grok-ref=' + JSON.stringify(ref) + ']');
  if (!el) return JSON.stringify({{ok:false, error:'ref not found: ' + ref}});
  try {{ el.scrollIntoView({{block:'center', inline:'nearest'}}); }} catch (e) {{}}
  var box = el.getBoundingClientRect();
  var opts = {{bubbles:true, cancelable:true, view:window, clientX: box.left + box.width/2, clientY: box.top + box.height/2}};
  try {{ el.dispatchEvent(new MouseEvent('mouseover', opts)); }} catch (e) {{}}
  try {{ el.dispatchEvent(new MouseEvent('mousemove', opts)); }} catch (e) {{}}
  return JSON.stringify({{ok:true, ref:ref}});
}})()"#
    )
}

pub fn select_js(r#ref: &str, value: &str) -> String {
    let r = js_str(r#ref);
    let v = js_str(value);
    format!(
        r#"(function(){{
  var ref = {r};
  var value = {v};
  var el = document.querySelector('[data-grok-ref=' + JSON.stringify(ref) + ']');
  if (!el) return JSON.stringify({{ok:false, error:'ref not found: ' + ref}});
  try {{ el.focus(); }} catch (e) {{}}
  if (el.tagName === 'SELECT') {{
    var opts = el.options || [];
    var hit = false;
    for (var i = 0; i < opts.length; i++) {{
      if (opts[i].value === value || opts[i].text === value) {{
        el.selectedIndex = i;
        hit = true;
        break;
      }}
    }}
    if (!hit) el.value = value;
    try {{ el.dispatchEvent(new Event('input', {{bubbles:true}})); }} catch (e) {{}}
    try {{ el.dispatchEvent(new Event('change', {{bubbles:true}})); }} catch (e) {{}}
    return JSON.stringify({{ok:true, ref:ref, value: el.value}});
  }}
  return JSON.stringify({{ok:false, error:'ref is not a <select>'}});
}})()"#
    )
}

#[allow(dead_code)]
pub fn press_js(key: &str) -> String {
    let k = js_str(key);
    format!(
        r#"(function(){{
  var key = {k};
  var el = document.activeElement || document.body;
  var opts = {{key:key, code:key, bubbles:true, cancelable:true}};
  try {{ el.dispatchEvent(new KeyboardEvent('keydown', opts)); }} catch (e) {{}}
  try {{ el.dispatchEvent(new KeyboardEvent('keypress', opts)); }} catch (e) {{}}
  try {{ el.dispatchEvent(new KeyboardEvent('keyup', opts)); }} catch (e) {{}}
  return JSON.stringify({{ok:true, key:key}});
}})()"#
    )
}

pub fn scroll_js(r#ref: Option<&str>, direction: &str, amount: i64) -> String {
    let r = r#ref.map(js_str).unwrap_or_else(|| "null".into());
    let d = js_str(direction);
    format!(
        r#"(function(){{
  var ref = {r};
  var direction = {d};
  var amount = {amount};
  var el = ref ? document.querySelector('[data-grok-ref=' + JSON.stringify(ref) + ']') : null;
  if (ref && !el) return JSON.stringify({{ok:false, error:'ref not found: ' + ref}});
  var dx = 0, dy = 0;
  if (direction === 'up') dy = -amount;
  else if (direction === 'down') dy = amount;
  else if (direction === 'left') dx = -amount;
  else if (direction === 'right') dx = amount;
  if (el) {{
    el.scrollTop += dy;
    el.scrollLeft += dx;
    try {{ el.scrollIntoView({{block:'center', inline:'nearest'}}); }} catch (e) {{}}
  }} else {{
    window.scrollBy(dx, dy);
  }}
  return JSON.stringify({{ok:true, direction:direction, amount:amount}});
}})()"#
    )
}

pub fn contains_text_js(text: &str) -> String {
    let t = js_str(text);
    format!(
        r#"(function(){{
  var needle = {t};
  var hay = ((document.body && (document.body.innerText || document.body.textContent)) || '') + '';
  return JSON.stringify({{ok:true, found: hay.indexOf(needle) >= 0, readyState: document.readyState || ''}});
}})()"#
    )
}

pub const READY_JS: &str = r#"(function(){
  return JSON.stringify({
    ok: true,
    readyState: document.readyState || '',
    href: location.href || '',
    title: document.title || ''
  });
})()"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_stamps_ref_attr() {
        assert!(SNAPSHOT_JS.contains("data-grok-ref"));
        assert!(SNAPSHOT_JS.contains("[ref="));
        assert!(SNAPSHOT_JS.contains("document"));
    }

    #[test]
    fn action_scripts_embed_json_ref() {
        let js = click_js("e12");
        assert!(js.contains("e12"));
        assert!(js.contains("data-grok-ref"));
        let typed = type_js("e3", "hello \"world\"", true);
        assert!(typed.contains("e3"));
        assert!(typed.contains("hello"));
        assert!(typed.contains("native"));
        assert!(select_js("e1", "cn").contains("SELECT"));
        assert!(press_js("Enter").contains("Enter"));
        assert!(focus_frame_js(Some("iframe")).contains("querySelector"));
        assert!(scroll_js(None, "down", 400).contains("window.scrollBy"));
        assert!(contains_text_js("登录").contains("登录"));
    }
}
