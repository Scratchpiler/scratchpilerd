import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { loadScratchpiler, toolsDir } from './js-bundle.mjs';
import { projects } from './projects.mjs';

const count = Number(process.argv[2] ?? 200);
const sp = await loadScratchpiler();
const binary = process.env.SCRATCHPILERD ?? path.join(toolsDir, '..', 'target', 'release', 'scratchpilerd');
const pa = sp.projectAnalysis;

function reference(item) {
    sp.scratchIndex.setScratchIndex(item.index);
    const project = pa.createProject();
    pa.setProjectIndex(project, item.index);
    for (const sprite of pa.spriteNamesOf(item.index)) if (item.files[sprite] !== undefined) pa.setProjectFile(project, sprite, item.files[sprite], item.index);
    const linked = pa.linkedProject(project, { monitored: new Set((item.index.monitored ?? []).map(([o, k, n]) => pa.varKey(o, k, n))) });
    const diags = linked.spriteNames.flatMap(sprite => pa.diagnosticsFor(linked, sprite).map(d => [sprite, d.line, d.col, d.len, d.severity, d.message, !!d.tags]));
    const items = linked.diagnostics.project.map(p => [p.message, p.varKey]);
    const counts = [...pa.messageCounts(linked)].map(([k, v]) => [k, v.senders, v.receivers]);
    const symbols = pa.projectSymbols(linked).map(s => [s.sprite, s.kind, String(s.name), s.label, s.line, s.col]);
    const graph = pa.eventGraph(linked);
    const g = {
        scripts: graph.scripts.map(s => [s.id, s.sprite, s.label, s.hat.event, s.line, s.col, s.reads, s.writes]),
        messages: graph.messages.map(m => [m.id, m.name, m.routine]),
        edges: graph.edges.map(e => [e.from, e.to, e.kind]),
    };
    const hits = [];
    for (const [sprite, file] of linked.files) {
        let n = 0;
        for (const t of file.analysis.tokens) {
            if (t.type !== 'STR' && t.type !== 'VAR') continue;
            if (t.line > file.lineCount || n++ > 150) continue;
            const hit = pa.projectSymbolAt(linked, sprite, t.line, t.col + 1);
            if (!hit) continue;
            const describe = hit.kind === 'message' ? pa.describeMessage(linked, hit.name) : pa.describeVariable(linked, hit.key);
            hits.push([sprite, t.line, t.col + 1, hit.kind, hit.kind === 'message' ? hit.name : hit.key, hit.span.line, hit.span.col, hit.span.endLine ?? null, hit.span.endCol ?? null,
                hit.kind === 'message' ? [describe.sentBy, describe.receivedBy] : [describe.writes, describe.reads]]);
        }
    }
    return JSON.stringify({ complete: linked.complete, clean: linked.clean, diags, items, counts, symbols, graph: g, hits });
}

const items = projects({ count });
const dir = fs.mkdtempSync(path.join(process.env.TMPDIR ?? '/tmp', 'scratchpilerd-project-'));
const lines = items.map((item, i) => {
    const sub = path.join(dir, String(i));
    fs.mkdirSync(sub);
    fs.writeFileSync(path.join(sub, 'files.json'), JSON.stringify(item.files));
    fs.writeFileSync(path.join(sub, 'index.json'), JSON.stringify(item.index));
    return sub;
});
fs.writeFileSync(path.join(dir, 'list.txt'), lines.join('\n'));
const start = performance.now();
const output = execFileSync(binary, ['debug', 'project', path.join(dir, 'list.txt')], { maxBuffer: 1 << 30 }).toString('utf8').split('\n');
const rustMs = performance.now() - start;
let mismatches = 0, crashes = 0;
const jsStart = performance.now();
items.forEach((item, i) => {
    let expected;
    try { expected = reference(item).replace(/\\ud[89ab][0-9a-f]{2}|\\ud[c-f][0-9a-f]{2}/g, '\ufffd'); } catch (error) { crashes++; return; }
    if (expected !== output[i]) {
        mismatches++;
        if (mismatches <= 3) {
            let k = 0;
            while (k < expected.length && expected[k] === output[i]?.[k]) k++;
            console.log(`MISMATCH ${item.name} (${lines[i]}) at ${k}\n  js:   ${expected.slice(Math.max(0, k - 300), k + 300)}\n  rust: ${output[i]?.slice(Math.max(0, k - 300), k + 300)}`);
        }
    }
});
console.log(`project: ${items.length} projects, ${mismatches} mismatches, ${crashes} skipped (JS threw); rust ${rustMs.toFixed(0)} ms, js ${(performance.now() - jsStart).toFixed(0)} ms`);
if (!mismatches) fs.rmSync(dir, { recursive: true });
process.exit(mismatches ? 1 : 0);
