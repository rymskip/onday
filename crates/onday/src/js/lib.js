(() => {
    const VERSION = '__ONDAY_VERSION__';
    const previous = window.__onday;
    const refs = previous && previous.refs ? previous.refs : new Map();
    const refOf = previous && previous.refOf ? previous.refOf : new WeakMap();
    let nextRef = previous && previous.nextRef ? previous.nextRef() : 1;

    const SKIP_TAGS = new Set(['script', 'style', 'noscript', 'template', 'head', 'meta', 'link', 'title']);

    function normalize(s) {
        return String(s == null ? '' : s).replace(/\s+/g, ' ').trim();
    }

    function childrenFlat(node) {
        const out = [];
        const root = node.shadowRoot;
        if (root) for (const c of root.children) out.push(c);
        const kids = node.children || [];
        for (const c of kids) out.push(c);
        if (node.tagName === 'SLOT' && node.assignedElements) for (const c of node.assignedElements()) out.push(c);
        return out;
    }

    function childNodesFlat(node) {
        const out = [];
        if (node.shadowRoot) for (const c of node.shadowRoot.childNodes) out.push(c);
        for (const c of node.childNodes) out.push(c);
        if (node.tagName === 'SLOT' && node.assignedNodes) for (const c of node.assignedNodes()) out.push(c);
        return out;
    }

    function describe(el) {
        if (!el || el.nodeType !== 1) return String(el);
        let s = '<' + el.tagName.toLowerCase();
        if (el.id) s += ' id="' + el.id + '"';
        for (const attr of ['data-testid', 'name', 'role', 'aria-label', 'type']) {
            const v = el.getAttribute(attr);
            if (v) s += ' ' + attr + '="' + v.slice(0, 40) + '"';
        }
        const cls = typeof el.className === 'string' ? el.className.trim() : '';
        if (cls) s += ' class="' + cls.slice(0, 60) + '"';
        return s + '>';
    }

    // ── visibility ────────────────────────────────────────────────────────
    function isVisible(el) {
        if (!el || !el.isConnected) return false;
        const cs = getComputedStyle(el);
        if (cs.visibility !== 'visible' || cs.display === 'none') return false;
        for (let p = el.parentElement; p; p = p.parentElement) {
            if (getComputedStyle(p).display === 'none') return false;
        }
        const r = el.getBoundingClientRect();
        return r.width > 0 && r.height > 0;
    }

    function isHiddenForAria(el) {
        if (el.hidden || el.getAttribute('aria-hidden') === 'true' || el.inert) return true;
        const cs = getComputedStyle(el);
        return cs.display === 'none' || cs.visibility === 'hidden' || cs.visibility === 'collapse';
    }

    // ── roles and accessible names ────────────────────────────────────────
    const INPUT_ROLES = {
        button: 'button', submit: 'button', reset: 'button', image: 'button', file: 'button', color: 'button',
        checkbox: 'checkbox', radio: 'radio', range: 'slider', number: 'spinbutton', search: 'searchbox',
        email: 'textbox', tel: 'textbox', text: 'textbox', url: 'textbox', password: 'textbox',
        date: 'textbox', 'datetime-local': 'textbox', month: 'textbox', time: 'textbox', week: 'textbox',
    };

    function hasExplicitName(el) {
        return !!(normalize(el.getAttribute('aria-label')) || el.getAttribute('aria-labelledby') || normalize(el.getAttribute('title')));
    }

    function insideSectioning(el) {
        return !!(el.parentElement && el.parentElement.closest('article, aside, main, nav, section'));
    }

    function implicitRole(el) {
        const tag = el.tagName.toLowerCase();
        switch (tag) {
            case 'a': case 'area': return el.hasAttribute('href') ? 'link' : null;
            case 'button': return 'button';
            case 'input': {
                const t = (el.getAttribute('type') || 'text').toLowerCase();
                if (t === 'hidden') return null;
                if (el.hasAttribute('list') && ['text', 'search', 'email', 'tel', 'url'].includes(t)) return 'combobox';
                return INPUT_ROLES[t] || 'textbox';
            }
            case 'textarea': return 'textbox';
            case 'select': return el.multiple || el.size > 1 ? 'listbox' : 'combobox';
            case 'option': return 'option';
            case 'optgroup': case 'fieldset': case 'details': case 'address': return 'group';
            case 'img': return el.getAttribute('alt') === '' ? 'presentation' : 'img';
            case 'h1': case 'h2': case 'h3': case 'h4': case 'h5': case 'h6': return 'heading';
            case 'ul': case 'ol': case 'menu': return 'list';
            case 'li': return 'listitem';
            case 'nav': return 'navigation';
            case 'main': return 'main';
            case 'aside': return 'complementary';
            case 'header': return insideSectioning(el) ? null : 'banner';
            case 'footer': return insideSectioning(el) ? null : 'contentinfo';
            case 'form': return hasExplicitName(el) ? 'form' : null;
            case 'section': return hasExplicitName(el) ? 'region' : null;
            case 'search': return 'search';
            case 'article': return 'article';
            case 'dialog': return 'dialog';
            case 'table': return 'table';
            case 'thead': case 'tbody': case 'tfoot': return 'rowgroup';
            case 'tr': return 'row';
            case 'td': return 'cell';
            case 'th': return el.getAttribute('scope') === 'row' ? 'rowheader' : 'columnheader';
            case 'caption': return 'caption';
            case 'progress': return 'progressbar';
            case 'meter': return 'meter';
            case 'hr': return 'separator';
            case 'p': return 'paragraph';
            case 'blockquote': return 'blockquote';
            case 'figure': return 'figure';
            case 'output': return 'status';
            case 'datalist': return 'listbox';
            case 'iframe': case 'frame': return 'iframe';
            case 'summary': return 'button';
            default: return null;
        }
    }

    function getRole(el) {
        const explicit = (el.getAttribute('role') || '').trim().split(/\s+/)[0];
        if (explicit) return explicit === 'none' ? 'presentation' : explicit;
        return implicitRole(el);
    }

    const NAME_FROM_CONTENT = new Set([
        'button', 'cell', 'checkbox', 'columnheader', 'gridcell', 'heading', 'link', 'menuitem',
        'menuitemcheckbox', 'menuitemradio', 'option', 'radio', 'row', 'rowheader', 'switch', 'tab',
        'tooltip', 'treeitem', 'caption', 'term', 'listitem',
    ]);

    function textFromContent(el, depth, exclude) {
        const out = [];
        for (const child of childNodesFlat(el)) {
            if (child.nodeType === 3) { out.push(child.textContent); continue; }
            if (child.nodeType !== 1 || child === exclude) continue;
            const tag = child.tagName.toLowerCase();
            if (SKIP_TAGS.has(tag) || isHiddenForAria(child)) continue;
            if (tag === 'input' || tag === 'textarea') { out.push(' ' + (child.value || '') + ' '); continue; }
            if (tag === 'select') { out.push(' ' + Array.from(child.selectedOptions).map(o => o.textContent).join(' ') + ' '); continue; }
            const aria = child.getAttribute('aria-label');
            if (aria) { out.push(' ' + aria + ' '); continue; }
            if (tag === 'img') { out.push(' ' + (child.getAttribute('alt') || '') + ' '); continue; }
            const display = getComputedStyle(child).display;
            const inline = display === 'inline' || display === 'contents';
            const inner = depth > 8 ? child.textContent : textFromContent(child, depth + 1, exclude);
            out.push(inline ? inner : ' ' + inner + ' ');
        }
        return out.join('');
    }

    function accName(el) {
        const labelledby = el.getAttribute('aria-labelledby');
        if (labelledby) {
            const text = labelledby.split(/\s+/).map(id => {
                const n = el.ownerDocument.getElementById(id);
                return n ? (n.getAttribute('aria-label') || textFromContent(n, 1)) : '';
            }).join(' ');
            if (normalize(text)) return normalize(text);
        }
        const label = el.getAttribute('aria-label');
        if (label && label.trim()) return normalize(label);
        const tag = el.tagName.toLowerCase();
        if (tag === 'input' || tag === 'textarea' || tag === 'select') {
            const t = (el.getAttribute('type') || '').toLowerCase();
            if (t === 'button' || t === 'submit' || t === 'reset') {
                return normalize(el.value || (t === 'submit' ? 'Submit' : t === 'reset' ? 'Reset' : ''));
            }
            if (t === 'image') return normalize(el.getAttribute('alt') || el.value || 'Submit');
            const labels = el.labels ? Array.from(el.labels) : [];
            if (labels.length) {
                const text = normalize(labels.map(l => textFromContent(l, 1, el)).join(' '));
                if (text) return text;
            }
            const title = el.getAttribute('title');
            if (title) return normalize(title);
            return normalize(el.getAttribute('placeholder'));
        }
        if (tag === 'img' || tag === 'area') {
            const alt = el.getAttribute('alt');
            if (alt) return normalize(alt);
        }
        if (tag === 'fieldset') {
            const legend = el.querySelector(':scope > legend');
            if (legend) return normalize(textFromContent(legend, 1));
        }
        if (tag === 'table') {
            const caption = el.querySelector(':scope > caption');
            if (caption) return normalize(textFromContent(caption, 1));
        }
        if (tag === 'figure') {
            const caption = el.querySelector(':scope > figcaption');
            if (caption) return normalize(textFromContent(caption, 1));
        }
        const role = getRole(el);
        if (NAME_FROM_CONTENT.has(role)) {
            const text = normalize(textFromContent(el, 0));
            if (text) return text;
        }
        const title = el.getAttribute('title');
        if (title) return normalize(title);
        if (tag === 'svg') {
            const t = el.querySelector(':scope > title');
            if (t) return normalize(t.textContent);
        }
        return '';
    }

    function checkedState(el, role) {
        if (!['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio'].includes(role)) return null;
        const aria = el.getAttribute('aria-checked');
        if (aria === 'mixed') return 'mixed';
        if (aria === 'true') return true;
        if (aria === 'false') return false;
        if (el.tagName === 'INPUT') return el.indeterminate ? 'mixed' : el.checked;
        return false;
    }

    function isDisabled(el) {
        if (el.getAttribute('aria-disabled') === 'true') return true;
        try { return el.matches(':disabled'); } catch (e) { return false; }
    }

    function states(el, role) {
        const out = [];
        const checked = checkedState(el, role);
        if (checked === true) out.push('checked');
        else if (checked === 'mixed') out.push('checked=mixed');
        if (isDisabled(el)) out.push('disabled');
        const expanded = el.getAttribute('aria-expanded');
        if (expanded === 'true') out.push('expanded');
        else if (expanded === 'false') out.push('expanded=false');
        if (el.getAttribute('aria-selected') === 'true' || (el.tagName === 'OPTION' && el.selected)) out.push('selected');
        const pressed = el.getAttribute('aria-pressed');
        if (pressed === 'true') out.push('pressed');
        else if (pressed === 'mixed') out.push('pressed=mixed');
        if (role === 'heading') {
            const level = el.getAttribute('aria-level') || (/^H([1-6])$/.exec(el.tagName) || [])[1];
            if (level) out.push('level=' + level);
        }
        if (el.required || el.getAttribute('aria-required') === 'true') out.push('required');
        if (el === document.activeElement && el !== document.body) out.push('active');
        return out;
    }

    const VALUE_ROLES = new Set(['textbox', 'searchbox', 'spinbutton', 'combobox', 'slider']);

    function valueOf(el, role) {
        if (!VALUE_ROLES.has(role)) return null;
        if (el.tagName === 'SELECT') return Array.from(el.selectedOptions).map(o => normalize(o.textContent)).join(', ');
        if ('value' in el && typeof el.value === 'string') return el.type === 'password' && el.value ? '••••' : el.value;
        if (el.isContentEditable) return normalize(el.textContent);
        const now = el.getAttribute('aria-valuenow');
        return now == null ? null : now;
    }

    function refFor(el) {
        let ref = refOf.get(el);
        if (!ref) {
            ref = 'e' + nextRef++;
            refOf.set(el, ref);
        }
        refs.set(ref, new WeakRef(el));
        return ref;
    }

    function byRef(ref) {
        const weak = refs.get(ref);
        const el = weak ? weak.deref() : null;
        return el && el.isConnected ? el : null;
    }

    // ── aria snapshot ─────────────────────────────────────────────────────
    const LEAF_ROLES = new Set(['textbox', 'searchbox', 'spinbutton', 'slider', 'img', 'separator', 'progressbar', 'meter', 'iframe']);

    function buildTree(node, out, budget) {
        for (const child of childNodesFlat(node)) {
            if (budget.left <= 0) return;
            if (child.nodeType === 3) {
                const text = normalize(child.textContent);
                if (text) out.push({ text });
                continue;
            }
            if (child.nodeType !== 1) continue;
            const tag = child.tagName.toLowerCase();
            if (SKIP_TAGS.has(tag) || isHiddenForAria(child)) continue;
            const role = getRole(child);
            if (!role || role === 'presentation' || role === 'generic') {
                buildTree(child, out, budget);
                continue;
            }
            budget.left--;
            const entry = { role, name: accName(child), ref: refFor(child), states: states(child, role), children: [] };
            const value = valueOf(child, role);
            if (value) entry.value = value;
            if (role === 'link' && child.getAttribute('href')) entry.url = child.getAttribute('href');
            if (!LEAF_ROLES.has(role)) buildTree(child, entry.children, budget);
            entry.children = mergeText(entry.children);
            if (entry.children.length === 1 && entry.children[0].text === entry.name) entry.children = [];
            out.push(entry);
        }
    }

    function mergeText(items) {
        const out = [];
        for (const item of items) {
            const last = out[out.length - 1];
            if (item.text !== undefined && last && last.text !== undefined) last.text += ' ' + item.text;
            else out.push(item);
        }
        return out;
    }

    function render(items, indent, lines) {
        const pad = '  '.repeat(indent);
        for (const item of items) {
            if (item.text !== undefined) {
                lines.push(pad + '- text: ' + JSON.stringify(item.text));
                continue;
            }
            let line = pad + '- ' + item.role;
            if (item.name) line += ' ' + JSON.stringify(item.name);
            for (const s of item.states) line += ' [' + s + ']';
            line += ' [ref=' + item.ref + ']';
            const extra = [];
            if (item.url) extra.push({ text: null, url: item.url });
            if (item.value && item.children.length) extra.push({ text: null, value: item.value });
            if (item.value && !item.children.length && !item.url) line += ': ' + JSON.stringify(item.value);
            const hasChildren = item.children.length || extra.length;
            lines.push(hasChildren ? line + ':' : line);
            for (const e of extra) {
                if (e.url) lines.push(pad + '  - /url: ' + e.url);
                if (e.value) lines.push(pad + '  - /value: ' + JSON.stringify(e.value));
            }
            render(item.children, indent + 1, lines);
        }
    }

    function snapshot(root, maxNodes) {
        const start = root || document.body || document.documentElement;
        const budget = { left: maxNodes || 5000 };
        const items = [];
        if (root && root.nodeType === 1 && !SKIP_TAGS.has(root.tagName.toLowerCase())) {
            // Walk a stand-in parent so the root itself is emitted, not just its children.
            buildTree({ childNodes: [root], shadowRoot: null, tagName: 'DIV' }, items, budget);
        } else {
            buildTree(start, items, budget);
        }
        const lines = [];
        render(mergeText(items), 0, lines);
        if (budget.left <= 0) lines.push('- text: "… snapshot truncated at ' + (maxNodes || 5000) + ' nodes; snapshot a ref to see more"');
        return lines.join('\n');
    }

    // ── selectors ─────────────────────────────────────────────────────────
    function splitChain(selector) {
        const parts = [];
        let current = '';
        let quote = null;
        let depth = 0;
        for (let i = 0; i < selector.length; i++) {
            const c = selector[i];
            if (quote) {
                current += c;
                if (c === '\\' && i + 1 < selector.length) { current += selector[++i]; continue; }
                if (c === quote) quote = null;
                continue;
            }
            if (c === '"' || c === "'") { quote = c; current += c; continue; }
            if (c === '[' || c === '(') depth++;
            if (c === ']' || c === ')') depth--;
            if (depth === 0 && c === '>' && selector[i + 1] === '>') {
                parts.push(current.trim());
                current = '';
                i++;
                continue;
            }
            current += c;
        }
        if (current.trim()) parts.push(current.trim());
        return parts;
    }

    const ENGINES = new Set(['css', 'text', 'testid', 'role', 'ref', 'nth', 'visible', 'has-text', 'xpath', 'id', 'label', 'placeholder', 'alt', 'title']);

    function parsePart(part) {
        const m = /^([a-z-]+)=([\s\S]*)$/.exec(part);
        if (m && ENGINES.has(m[1])) return { engine: m[1], body: m[2].trim() };
        return { engine: 'css', body: part };
    }

    function unquote(body) {
        const m = /^(["'])([\s\S]*)\1$/.exec(body);
        return m ? m[2].replace(/\\(.)/g, '$1') : body;
    }

    function textMatcher(body) {
        const re = /^\/([\s\S]*)\/([a-z]*)$/.exec(body);
        if (re) {
            const regex = new RegExp(re[1], re[2]);
            return t => regex.test(t);
        }
        const exact = /^(["'])([\s\S]*)\1([is]?)$/.exec(body);
        if (exact) {
            const want = normalize(exact[2].replace(/\\(.)/g, '$1'));
            if (exact[3] === 'i') return t => normalize(t).toLowerCase() === want.toLowerCase();
            return t => normalize(t) === want;
        }
        const needle = normalize(body).toLowerCase();
        return t => normalize(t).toLowerCase().includes(needle);
    }

    function elementText(el) {
        const tag = el.tagName;
        if (tag === 'INPUT' && ['button', 'submit', 'reset'].includes((el.type || '').toLowerCase())) return el.value;
        if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return '';
        return typeof el.innerText === 'string' ? el.innerText : el.textContent;
    }

    function rootElement(root) {
        if (root === document) return document.documentElement;
        return root;
    }

    function deepQuery(root, css) {
        const out = [];
        const visit = (scope) => {
            for (const el of scope.querySelectorAll(css)) out.push(el);
            for (const el of scope.querySelectorAll('*')) if (el.shadowRoot) visit(el.shadowRoot);
        };
        visit(root);
        return out;
    }

    function allElements(root) {
        const out = [];
        const visit = (el) => {
            for (const c of childrenFlat(el)) { out.push(c); visit(c); }
        };
        visit(rootElement(root));
        return out;
    }

    function textEngine(root, body) {
        const matcher = textMatcher(body);
        const plain = /^\/|^["']/.test(body) ? null : normalize(body).toLowerCase();
        const out = [];
        const walk = (el) => {
            const tag = el.tagName.toLowerCase();
            if (SKIP_TAGS.has(tag)) return false;
            // Prune subtrees that cannot contain the text; button-like inputs carry it in `value`.
            if (plain && !el.shadowRoot && tag !== 'input'
                && !(el.textContent || '').toLowerCase().includes(plain)
                && !el.querySelector('input[type=button], input[type=submit], input[type=reset]')) {
                return false;
            }
            let childMatched = false;
            for (const c of childrenFlat(el)) if (walk(c)) childMatched = true;
            if (childMatched) return true;
            const text = elementText(el);
            if (text && matcher(text)) { out.push(el); return true; }
            return false;
        };
        const start = rootElement(root);
        if (start.nodeType === 1) walk(start);
        else for (const c of start.children) walk(c);
        return out;
    }

    function parseRole(body) {
        const m = /^([a-z]+)([\s\S]*)$/i.exec(body);
        if (!m) throw new Error('bad role selector: ' + body);
        const spec = { role: m[1].toLowerCase(), attrs: [] };
        const attrRe = /\[\s*([a-z-]+)\s*(?:=\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\/(?:[^\/\\]|\\.)*\/[a-z]*|[^\]\s]+)\s*([is])?)?\s*\]/gi;
        let a;
        while ((a = attrRe.exec(m[2]))) spec.attrs.push({ name: a[1].toLowerCase(), value: a[2], flag: a[3] });
        return spec;
    }

    function roleEngine(root, body) {
        const spec = parseRole(body);
        let nameMatch = null;
        let includeHidden = false;
        const stateChecks = [];
        for (const attr of spec.attrs) {
            if (attr.name === 'name') {
                const raw = attr.value || '';
                if (raw.startsWith('/')) nameMatch = textMatcher(raw);
                else {
                    const text = unquote(raw);
                    const exact = attr.flag === 's' || spec.attrs.some(x => x.name === 'exact');
                    nameMatch = exact ? (t => t === normalize(text)) : (t => t.toLowerCase().includes(normalize(text).toLowerCase()));
                }
            } else if (attr.name === 'include-hidden') {
                includeHidden = attr.value === undefined || attr.value === 'true';
            } else if (attr.name !== 'exact') {
                const want = attr.value === undefined ? 'true' : unquote(attr.value);
                stateChecks.push({ name: attr.name, want });
            }
        }
        const hiddenByAncestor = (el) => {
            for (let p = el; p; p = p.parentElement || (p.getRootNode && p.getRootNode().host)) {
                if (p.nodeType === 1 && isHiddenForAria(p)) return true;
            }
            return false;
        };
        return allElements(root).filter(el => {
            if (getRole(el) !== spec.role) return false;
            if (!includeHidden && hiddenByAncestor(el)) return false;
            if (nameMatch && !nameMatch(accName(el))) return false;
            const st = states(el, spec.role);
            const stateValue = (name) => {
                if (st.includes(name)) return 'true';
                const pair = st.find(s => s.startsWith(name + '='));
                return pair ? pair.slice(name.length + 1) : 'false';
            };
            return stateChecks.every(check => stateValue(check.name) === check.want);
        });
    }

    function attrEngine(root, attr, body) {
        const matcher = textMatcher(body);
        return deepQuery(root, '[' + attr + ']').filter(el => matcher(el.getAttribute(attr) || ''));
    }

    function labelEngine(root, body) {
        const matcher = textMatcher(body);
        return allElements(root).filter(el => {
            const tag = el.tagName;
            const control = tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable
                || el.hasAttribute('aria-labelledby') || el.hasAttribute('aria-label');
            if (!control || (tag === 'INPUT' && el.type === 'hidden')) return false;
            const name = accName(el);
            return name && matcher(name);
        });
    }

    function queryEngine(engine, body, root, testIdAttr) {
        switch (engine) {
            case 'css': return deepQuery(root, body);
            case 'xpath': {
                const out = [];
                const result = document.evaluate(body, root, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
                for (let i = 0; i < result.snapshotLength; i++) out.push(result.snapshotItem(i));
                return out;
            }
            case 'id': return deepQuery(root, '[id="' + CSS.escape(unquote(body)) + '"]');
            case 'testid': return deepQuery(root, '[' + testIdAttr + '="' + unquote(body).replace(/\\/g, '\\\\').replace(/"/g, '\\"') + '"]');
            case 'text': return textEngine(root, body);
            case 'role': return roleEngine(root, body);
            case 'label': return labelEngine(root, body);
            case 'placeholder': return attrEngine(root, 'placeholder', body);
            case 'alt': return attrEngine(root, 'alt', body);
            case 'title': return attrEngine(root, 'title', body);
            case 'ref': {
                const el = byRef(unquote(body));
                return el && (root === document || rootElement(root).contains(el)) ? [el] : [];
            }
            default: throw new Error('unknown selector engine ' + engine);
        }
    }

    function resolve(selector, testIdAttr, scope) {
        let current = [scope || document];
        for (const part of splitChain(selector)) {
            const { engine, body } = parsePart(part);
            if (engine === 'nth') {
                const n = parseInt(body, 10);
                const idx = n < 0 ? current.length + n : n;
                current = idx >= 0 && idx < current.length ? [current[idx]] : [];
                continue;
            }
            if (engine === 'visible') {
                const want = body !== 'false';
                current = current.filter(e => isVisible(e) === want);
                continue;
            }
            if (engine === 'has-text') {
                const matcher = textMatcher(body);
                current = current.filter(e => matcher(elementText(e) || ''));
                continue;
            }
            const seen = new Set();
            const next = [];
            for (const root of current) {
                for (const el of queryEngine(engine, body, root, testIdAttr)) {
                    if (!seen.has(el)) { seen.add(el); next.push(el); }
                }
            }
            current = next;
        }
        return current.filter(n => n && n.nodeType === 1);
    }

    // ── actionability ─────────────────────────────────────────────────────
    function probe(el, hoverSel, requireEnabled) {
        if (!el || !el.isConnected) return { ok: false, reason: 'detached from the document' };
        const hover = hoverSel ? !!el.closest(hoverSel) : false;
        const rect = el.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) return { ok: false, reason: 'zero size' };
        const cs = getComputedStyle(el);
        if (cs.visibility !== 'visible') return { ok: false, reason: 'hidden (visibility ' + cs.visibility + ')' };
        if (!hover && parseFloat(cs.opacity) < 0.99) return { ok: false, reason: 'transparent (opacity ' + cs.opacity + ')' };
        for (let p = el.parentElement; p && p !== document.body; p = p.parentElement) {
            const pcs = getComputedStyle(p);
            if (pcs.visibility === 'hidden') return { ok: false, reason: 'ancestor hidden: ' + describe(p) };
            const exempt = hoverSel ? p.matches(hoverSel) : false;
            if (!exempt && parseFloat(pcs.opacity) < 0.99) {
                return {
                    ok: false,
                    reason: 'ancestor transparent: ' + describe(p) + ' data-state=' + p.getAttribute('data-state') +
                        ' opacity=' + pcs.opacity + ' animation=' + pcs.animationName + '/' + pcs.animationFillMode +
                        ' running=' + (p.getAnimations ? p.getAnimations().length : -1),
                };
            }
        }
        if (requireEnabled && isDisabled(el)) return { ok: false, reason: 'disabled' };
        return { ok: true, rect: [rect.x, rect.y, rect.width, rect.height] };
    }

    function prescroll(el) {
        /*__PRESCROLL__*/
    }

    function deepHit(x, y) {
        let hit = document.elementFromPoint(x, y);
        while (hit && hit.shadowRoot) {
            const inner = hit.shadowRoot.elementFromPoint(x, y);
            if (!inner || inner === hit) break;
            hit = inner;
        }
        return hit;
    }

    function composedContains(ancestor, node) {
        for (let n = node; n; n = n.parentNode || (n.host || null)) {
            if (n === ancestor) return true;
        }
        return false;
    }

    function hitPoint(el) {
        if (!el || !el.isConnected) return { ok: false, reason: 'detached from the document', x: 0, y: 0, outside: false };
        const r = el.getBoundingClientRect();
        const left = Math.max(r.left, 0), top = Math.max(r.top, 0);
        const right = Math.min(r.right, window.innerWidth), bottom = Math.min(r.bottom, window.innerHeight);
        if (right <= left || bottom <= top) {
            return { ok: false, reason: 'outside the viewport', x: Math.round(r.left), y: Math.round(r.top), outside: true };
        }
        const x = Math.floor((left + right) / 2), y = Math.floor((top + bottom) / 2);
        const hit = deepHit(x, y);
        if (!hit) return { ok: false, reason: 'nothing at (' + x + ', ' + y + ')', x, y, outside: false };
        if (composedContains(el, hit) || composedContains(hit, el)) return { ok: true, reason: '', x, y, outside: false };
        if (hit.tagName === 'LABEL' && hit.control === el) return { ok: true, reason: '', x, y, outside: false };
        return { ok: false, reason: describe(hit) + ' intercepts pointer events at (' + x + ', ' + y + ')', x, y, outside: false };
    }

    function scrollIntoViewIfNeeded(el) {
        el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    }

    // ── form helpers ──────────────────────────────────────────────────────
    function clearForTyping(el) {
        el.focus();
        const tag = el.tagName;
        if (tag === 'INPUT' || tag === 'TEXTAREA') {
            const proto = tag === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
            const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
            setter.call(el, '');
            el.dispatchEvent(new Event('input', { bubbles: true }));
            return 'input';
        }
        if (el.isContentEditable) {
            const range = document.createRange();
            range.selectNodeContents(el);
            const sel = window.getSelection();
            sel.removeAllRanges();
            sel.addRange(range);
            return 'editable';
        }
        return 'other';
    }

    // Inputs whose UI does not accept typed characters take their value directly.
    const DIRECT_TYPES = new Set(['date', 'time', 'datetime-local', 'month', 'week', 'color', 'range']);

    function fillDirect(el, value) {
        if (el.tagName !== 'INPUT' || !DIRECT_TYPES.has((el.type || '').toLowerCase())) return false;
        el.focus();
        const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
        setter.call(el, value);
        if (value && el.value !== value) throw new Error('malformed value ' + JSON.stringify(value) + ' for ' + describe(el));
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return true;
    }

    function selectOptions(el, values) {
        if (el.tagName !== 'SELECT') throw new Error('not a <select>: ' + describe(el));
        const chosen = [];
        for (const option of el.options) {
            const hit = values.some(v => option.value === v || normalize(option.label) === v || normalize(option.textContent) === v);
            if (hit && (el.multiple || chosen.length === 0)) {
                option.selected = true;
                chosen.push(option.value);
            } else {
                option.selected = false;
            }
        }
        if (values.length && !chosen.length) throw new Error('no option of ' + describe(el) + ' matches ' + JSON.stringify(values));
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return chosen;
    }

    function info(el) {
        const r = el.getBoundingClientRect();
        return {
            tag: el.tagName.toLowerCase(),
            text: normalize(elementText(el) || ''),
            value: 'value' in el && typeof el.value === 'string' ? el.value : null,
            checked: typeof el.checked === 'boolean' ? el.checked : null,
            visible: isVisible(el),
            enabled: !isDisabled(el),
            editable: !isDisabled(el) && !el.readOnly && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable),
            rect: [r.x, r.y, r.width, r.height],
            description: describe(el),
        };
    }

    // ── classic-protocol capture (BiDi sessions use native events) ─────────
    function installCapture() {
        if (window.__ondayCapture) return false;
        window.__ondayCapture = true;
        const restore = (key) => {
            try {
                const parsed = JSON.parse(sessionStorage.getItem(key) || '[]');
                return Array.isArray(parsed) ? parsed : [];
            } catch (e) { return []; }
        };
        const CAP = 1000;
        const buffers = { console: restore('__ondayConsole'), network: restore('__ondayNetwork') };
        const record = (kind, entry) => {
            const buffer = buffers[kind];
            buffer.push(entry);
            if (buffer.length > CAP) buffer.splice(0, buffer.length - CAP);
            try { sessionStorage.setItem(kind === 'console' ? '__ondayConsole' : '__ondayNetwork', JSON.stringify(buffer)); } catch (e) { /* quota or privacy mode */ }
        };
        window.__ondayBuffers = buffers;
        window.__ondayPersist = () => {
            try {
                sessionStorage.setItem('__ondayConsole', JSON.stringify(buffers.console));
                sessionStorage.setItem('__ondayNetwork', JSON.stringify(buffers.network));
            } catch (e) { /* quota or privacy mode */ }
        };
        const fmt = (a) => {
            if (a === null || a === undefined) return String(a);
            if (typeof a === 'object') { try { return JSON.stringify(a); } catch (e) { return String(a); } }
            return String(a);
        };
        for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
            const original = console[level];
            console[level] = function (...args) {
                record('console', { level: level === 'log' ? 'info' : level, text: args.map(fmt).join(' '), time: Date.now() });
                return original.apply(console, args);
            };
        }
        window.addEventListener('error', (e) => record('console', { level: 'error', text: 'Uncaught ' + e.message + ' at ' + e.filename + ':' + e.lineno, time: Date.now() }));
        window.addEventListener('unhandledrejection', (e) => record('console', { level: 'error', text: 'Unhandled rejection: ' + fmt(e.reason), time: Date.now() }));
        const originalFetch = window.fetch;
        window.fetch = function (input, init) {
            const method = ((init && init.method) || (typeof input === 'object' && input.method) || 'GET').toUpperCase();
            const url = typeof input === 'string' ? input : (input && input.url) || String(input);
            const started = Date.now();
            return originalFetch.apply(window, arguments).then((res) => {
                record('network', { method, url: res.url || url, status: res.status, error: null, durationMs: Date.now() - started, time: started });
                return res;
            }, (err) => {
                record('network', { method, url, status: null, error: String(err), durationMs: Date.now() - started, time: started });
                throw err;
            });
        };
        const open = XMLHttpRequest.prototype.open;
        const send = XMLHttpRequest.prototype.send;
        XMLHttpRequest.prototype.open = function (method, url) {
            this.__onday = { method: String(method).toUpperCase(), url: String(url) };
            return open.apply(this, arguments);
        };
        XMLHttpRequest.prototype.send = function () {
            const meta = this.__onday;
            if (meta) {
                const started = Date.now();
                this.addEventListener('loadend', () => record('network', {
                    method: meta.method, url: this.responseURL || meta.url, status: this.status || null,
                    error: this.status ? null : 'request failed', durationMs: Date.now() - started, time: started,
                }));
            }
            return send.apply(this, arguments);
        };
        return true;
    }

    function drainCapture() {
        const buffers = window.__ondayBuffers;
        if (!buffers) return { console: [], network: [] };
        const out = { console: buffers.console.splice(0), network: buffers.network.splice(0) };
        window.__ondayPersist();
        return out;
    }

    window.__onday = {
        version: VERSION,
        refs,
        refOf,
        nextRef: () => nextRef,
        resolve,
        snapshot,
        byRef,
        refFor,
        probe,
        prescroll,
        hitPoint,
        scrollIntoViewIfNeeded,
        clearForTyping,
        fillDirect,
        selectOptions,
        info,
        describe,
        isVisible,
        accName,
        getRole,
        installCapture,
        drainCapture,
    };
})();
