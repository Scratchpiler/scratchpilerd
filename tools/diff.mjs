import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { loadScratchpiler, toolsDir } from './js-bundle.mjs';
import { programs } from './programs.mjs';

const mode = process.argv[2] ?? 'ast';
const seeds = Number(process.argv[3] ?? 300);
const sp = await loadScratchpiler();
const binary = process.env.SCRATCHPILERD ?? path.join(toolsDir, '..', 'target', 'release', 'scratchpilerd');

const headerDir = path.join(toolsDir, '..', '..', 'scratchpiler', 'run', 'catos', 'src', 'include');
for (const file of fs.existsSync(headerDir) ? fs.readdirSync(headerDir).filter(f => f.endsWith('.h')) : []) {
    sp.headers.writeHeader(file, fs.readFileSync(path.join(headerDir, file), 'utf8'));
}

function markers(text, items) {
    const lines = text.split('\n');
    return items.map(item => {
        const line = Math.min(item.line, lines.length);
        const endColumn = item.wholeLine ? lines[line - 1].length + 1 : item.col + (item.len || 1);
        return [line, item.col, endColumn, item.severity, item.message];
    });
}

const reference = {
    diag(text, item) {
        sp.scratchIndex.setScratchIndex(item.index);
        const a = sp.analyzer.analyze(text, item.sprite);
        const severity = level => d => ({ ...d, severity: level });
        const items = [
            ...a.parseErrors.map(severity('error')),
            ...sp.compiler.lint(a.ast).map(w => ({ ...w, severity: 'warning', wholeLine: w.category === 'Unreachable' || w.category === 'Orphaned' })),
            ...sp.compiler.typeCheckDiagnostics(a.ast, item.sprite).map(severity('warning')),
            ...sp.analyzer.semanticDiagnostics(a).map(severity('warning')),
            ...sp.analyzer.smellDiagnostics(a).map(severity('info')),
        ];
        const tokens = sp.analyzer.buildSemanticTokens(a).map(t => [t.line, t.col, t.length, t.tokenType]);
        const occ = a.occurrences.map(o => [o.line, o.col, o.endLine, o.endCol, o.symbol.kind, String(o.symbol.name), !!o.isDef, o.access ?? null]);
        return JSON.stringify({ markers: markers(text, items), tokens, occ });
    },
    ast(text) {
        const tokens = sp.compiler.tokenize(text);
        const { ast, errors } = sp.compiler.parse(tokens);
        return JSON.stringify({ ast, errors: errors.map(e => ({ line: e.line, col: e.col, len: e.len || 1, message: e.message })) });
    },
};

const normalizeSurrogates = json => json.replace(/\\ud[89ab][0-9a-f]{2}|\\ud[c-f][0-9a-f]{2}/g, '\ufffd');

const items = programs({ seeds });
const dir = fs.mkdtempSync(path.join(process.env.TMPDIR ?? '/tmp', 'scratchpilerd-diff-'));
const list = [];
const lines = [];
const indexFiles = new Map();
items.forEach((item, i) => {
    const file = path.join(dir, `${i}.sdsl`);
    fs.writeFileSync(file, item.text);
    list.push(file);
    const indexJson = JSON.stringify(item.index ?? {});
    if (!indexFiles.has(indexJson)) {
        const indexFile = path.join(dir, `index-${indexFiles.size}.json`);
        fs.writeFileSync(indexFile, indexJson);
        indexFiles.set(indexJson, indexFile);
    }
    lines.push([file, indexFiles.get(indexJson), item.sprite ?? 'Sprite1', headerDir].join('\t'));
});
fs.writeFileSync(path.join(dir, 'list.txt'), lines.join('\n'));
const output = execFileSync(binary, ['debug', mode, path.join(dir, 'list.txt')], { maxBuffer: 1 << 30 }).toString('utf8').split('\n');

let mismatches = 0, crashes = 0;
items.forEach((item, i) => {
    let expected;
    try { expected = normalizeSurrogates(reference[mode](item.text, item)); } catch (error) { crashes++; return; }
    if (expected !== output[i]) {
        mismatches++;
        if (mismatches <= 3) {
            let k = 0;
            while (k < expected.length && expected[k] === output[i]?.[k]) k++;
            console.log(`MISMATCH ${item.name} (${list[i]}) at ${k}\n  js:   ${expected.slice(Math.max(0, k - 200), k + 200)}\n  rust: ${output[i]?.slice(Math.max(0, k - 200), k + 200)}`);
        }
    }
});
console.log(`${mode}: ${items.length} programs, ${mismatches} mismatches, ${crashes} skipped (JS threw)`);
if (!mismatches) fs.rmSync(dir, { recursive: true });
process.exit(mismatches ? 1 : 0);
