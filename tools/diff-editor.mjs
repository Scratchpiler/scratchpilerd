import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { loadScratchpiler, fakeMonaco, toolsDir } from './js-bundle.mjs';
import { programs, mulberry32 } from './programs.mjs';

const seeds = Number(process.argv[2] ?? 100);
const sp = await loadScratchpiler(`export * as editor from './editor.js';`);
const providers = {};
sp.language.registerLanguage(fakeMonaco((kind, args) => { providers[kind] ??= args[1]; }));
const binary = process.env.SCRATCHPILERD ?? path.join(toolsDir, '..', 'target', 'release', 'scratchpilerd');
const headerDir = path.join(toolsDir, '..', '..', 'scratchpiler', 'run', 'catos', 'src', 'include');
for (const file of fs.existsSync(headerDir) ? fs.readdirSync(headerDir).filter(f => f.endsWith('.h')) : []) sp.headers.writeHeader(file, fs.readFileSync(path.join(headerDir, file), 'utf8'));

const WORD = /(-?\d*\.\d\w*)|([^`~!@#$%^&*()\-=+[{\]}\\|;:'",.<>/?\s]+)/g;
function wordAt(line, column) {
    WORD.lastIndex = 0;
    for (let m; (m = WORD.exec(line));) {
        if (m.index <= column - 1 && column - 1 <= m.index + m[0].length) return { word: m[0], startColumn: m.index + 1, endColumn: m.index + 1 + m[0].length };
        if (m.index > column) break;
    }
    return null;
}
function makeModel(text, version) {
    const lines = text.split('\n');
    const starts = [0];
    for (const l of lines) starts.push(starts.at(-1) + l.length + 1);
    return {
        getValue: () => text, getVersionId: () => version, getLineContent: n => lines[n - 1], getLineCount: () => lines.length,
        getOffsetAt: p => starts[p.lineNumber - 1] + p.column - 1,
        getWordAtPosition: p => wordAt(lines[p.lineNumber - 1], p.column),
        getWordUntilPosition: p => {
            const w = wordAt(lines[p.lineNumber - 1], p.column);
            return w ? { word: w.word.slice(0, p.column - w.startColumn), startColumn: w.startColumn, endColumn: p.column } : { word: '', startColumn: p.column, endColumn: p.column };
        },
        uri: { toString: () => `inmemory://editor-${version}` },
    };
}

const items = programs({ seeds, mutationsPer: 1 });
const rng = mulberry32(99);
const dir = fs.mkdtempSync(path.join(process.env.TMPDIR ?? '/tmp', 'scratchpilerd-editor-'));
const listLines = [];
const meta = [];
let version = 0;
items.forEach((item, i) => {
    const lines = item.text.split('\n');
    const positions = [];
    const budget = Math.min(60, lines.length * 3);
    for (let k = 0; k < budget; k++) {
        const line = 1 + Math.floor(rng() * lines.length);
        const column = 1 + Math.floor(rng() * (lines[line - 1].length + 1));
        const before = lines[line - 1][column - 2];
        const trigger = before && '"([.<'.includes(before) && rng() < 0.7 ? before : null;
        positions.push([line, column, trigger]);
    }
    for (let k = 0; k < budget / 2; k++) {
        const line = 1 + Math.floor(rng() * lines.length);
        const text = lines[line - 1];
        const spots = [...text.matchAll(/["\[.(<]|\w\b|\s$/g)].map(m => m.index + 2);
        if (!spots.length) continue;
        const column = Math.min(spots[Math.floor(rng() * spots.length)], text.length + 1);
        const before = text[column - 2];
        positions.push([line, column, before && '"([.<'.includes(before) ? before : null]);
    }
    const index = { ...item.index };
    index.sprites = (index.sprites ?? []).map(s => ({ ...s, costumes: [], sounds: [] }));
    index.stage = { backdrops: index.stage?.backdrops ?? [], sounds: [] };
    index.broadcasts = [];
    const file = path.join(dir, `${i}.sdsl`);
    fs.writeFileSync(file, item.text);
    fs.writeFileSync(path.join(dir, `${i}.index.json`), JSON.stringify(index));
    fs.writeFileSync(path.join(dir, `${i}.pos.json`), JSON.stringify(positions));
    listLines.push([file, path.join(dir, `${i}.index.json`), item.sprite ?? 'Sprite1', headerDir, path.join(dir, `${i}.pos.json`)].join('\t'));
    meta.push({ index, positions });
});
fs.writeFileSync(path.join(dir, 'list.txt'), listLines.join('\n'));
const outFile = path.join(dir, 'rust.jsonl');
const fd = fs.openSync(outFile, 'w');
execFileSync(binary, ['debug', 'editor', path.join(dir, 'list.txt')], { stdio: ['ignore', fd, 'inherit'] });
fs.closeSync(fd);
const { createInterface } = await import('node:readline');
const lineReader = createInterface({ input: fs.createReadStream(outFile), crlfDelay: Infinity })[Symbol.asyncIterator]();

function reference(item, { index, positions }) {
    sp.scratchIndex.setScratchIndex(index);
    sp.editor.selectSidebarSprite(item.sprite ?? 'Sprite1');
    const model = makeModel(item.text, ++version);
    const rows = positions.map(([lineNumber, column, trigger]) => {
        const position = { lineNumber, column };
        let completion;
        try {
            const result = providers.registerCompletionItemProvider.provideCompletionItems(model, position, { triggerKind: trigger ? 1 : 0, triggerCharacter: trigger ?? undefined });
            completion = (result?.suggestions ?? []).map(s => [s.label, s.kind, s.detail ?? null, s.insertText, s.range.startColumn, s.range.endColumn, s.filterText ?? null, s.sortText ?? null, s.command?.id ?? null]);
        } catch (e) { completion = 'THREW'; }
        let h = null;
        try { h = providers.registerHoverProvider.provideHover(model, position); } catch { h = null; }
        const hover = h ? [{ start: { line: h.range.startLineNumber - 1, character: h.range.startColumn - 1 }, end: { line: h.range.endLineNumber - 1, character: h.range.endColumn - 1 } }, h.contents.map(c => c.value)] : null;
        let s = null;
        try { s = providers.registerSignatureHelpProvider.provideSignatureHelp(model, position)?.value; } catch { s = null; }
        const sig = s ? { signatures: s.signatures.map(x => ({ label: x.label, parameters: x.parameters.map(p => p.documentation ? { label: String(p.label), documentation: p.documentation } : { label: String(p.label) }) })), activeSignature: s.activeSignature, activeParameter: s.activeParameter } : null;
        return [completion, hover, sig];
    });
    return rows;
}

const canon = v => JSON.stringify(v, (k, x) => x && typeof x === 'object' && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort()) : x);
let mismatches = 0, total = 0, skipped = 0;
const kinds = { completion: 0, hover: 0, sig: 0 };
for (const [i, item] of items.entries()) {
    const rust = JSON.parse((await lineReader.next()).value);
    reference(item, meta[i]).forEach((row, k) => {
        total++;
        if (row[0] === 'THREW') { skipped++; return; }
        ['completion', 'hover', 'sig'].forEach((name, j) => {
            const a = canon(row[j]), b = canon(rust[k][j]);
            if (a !== b) {
                kinds[name]++;
                if (++mismatches <= 4 || (name !== 'completion' && kinds[name] <= 2)) {
                    let c = 0;
                    while (c < a.length && a[c] === b[c]) c++;
                    const [line, col] = JSON.parse(fs.readFileSync(path.join(dir, `${i}.pos.json`)))[k];
                    console.log(`MISMATCH ${name} ${item.name} @${line}:${col} line=${JSON.stringify(item.text.split('\n')[line - 1])}\n  js:   ${a.slice(Math.max(0, c - 150), c + 250)}\n  rust: ${b.slice(Math.max(0, c - 150), c + 250)}`);
                }
            }
        });
    });
}
console.log(`editor: ${total} positions in ${items.length} files, ${mismatches} mismatches ${JSON.stringify(kinds)}, ${skipped} skipped`);
if (!mismatches) fs.rmSync(dir, { recursive: true });
process.exit(mismatches ? 1 : 0);
