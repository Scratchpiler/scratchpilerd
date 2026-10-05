import fs from 'node:fs';
import path from 'node:path';
import { startServer, uriOf } from './lsp-client.mjs';

const projects = process.argv.slice(2);
const idleSeconds = Number(process.env.IDLE_SECONDS ?? 60);
const sleep = ms => new Promise(r => setTimeout(r, ms));

function cpuNs(pid) {
    let total = 0;
    for (const tid of fs.readdirSync(`/proc/${pid}/task`)) {
        try { total += Number(fs.readFileSync(`/proc/${pid}/task/${tid}/schedstat`, 'utf8').split(' ')[0]); } catch {}
    }
    return total;
}
function wakeups(pid) {
    let total = 0;
    for (const tid of fs.readdirSync(`/proc/${pid}/task`)) {
        try {
            const status = fs.readFileSync(`/proc/${pid}/task/${tid}/status`, 'utf8');
            total += Number(/voluntary_ctxt_switches:\s+(\d+)/.exec(status)[1]) + Number(/nonvoluntary_ctxt_switches:\s+(\d+)/.exec(status)[1]);
        } catch {}
    }
    return total;
}
function memory(pid) {
    const status = fs.readFileSync(`/proc/${pid}/status`, 'utf8');
    const kb = key => Number(new RegExp(`${key}:\\s+(\\d+)`).exec(status)[1]);
    return { rss: kb('VmRSS') / 1024, peak: kb('VmHWM') / 1024, threads: kb('Threads') };
}

async function analysisRun(project, cacheDir, { openLargest = false, interactive = false } = {}) {
    const start = performance.now();
    const server = startServer(['--project', project, '--cache-dir', cacheDir]);
    const pid = server.child.pid;
    await server.request('initialize', { processId: process.pid, capabilities: { window: { workDoneProgress: true } } });
    server.notify('initialized', {});
    const files = fs.readdirSync(project).filter(f => f.endsWith('.sdsl'));
    const latencies = [];
    if (interactive) {
        const largest = files.map(f => [f, fs.statSync(path.join(project, f)).size]).sort((a, b) => b[1] - a[1])[0][0];
        const file = path.join(project, largest);
        const text = fs.readFileSync(file, 'utf8');
        server.notify('textDocument/didOpen', { textDocument: { uri: uriOf(file), languageId: 'scratchpiler', version: 1, text } });
        const lines = text.split('\n');
        const editStart = performance.now();
        server.notify('textDocument/didChange', { textDocument: { uri: uriOf(file), version: 2 }, contentChanges: [{ range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } }, text: '// edited\n' }] });
        await server.waitFor(m => m.method === 'textDocument/publishDiagnostics' && m.params.uri === uriOf(file) && m.params.version === 2, 60000);
        latencies.edit = performance.now() - editStart;
        for (let i = 0; i < 40; i++) {
            const line = Math.floor(lines.length * i / 40);
            const t = performance.now();
            await server.request(i % 2 ? 'textDocument/hover' : 'textDocument/completion', { textDocument: { uri: uriOf(file) }, position: { line, character: Math.min(8, lines[line].length) } });
            latencies.push(performance.now() - t);
        }
    }
    let last = performance.now();
    let seen = new Set();
    for (;;) {
        const status = (await server.request('scratchpiler/status', null)).result;
        const published = server.notifications.filter(m => m.method === 'textDocument/publishDiagnostics');
        seen = new Set(published.map(m => m.params.uri));
        if (published.length) last = Math.max(last, performance.now());
        if (status.ready && seen.size >= files.length) break;
        await sleep(2);
    }
    const done = performance.now() - start;
    await sleep(300);
    const mem = memory(pid);
    const cpu = cpuNs(pid);
    return { server, pid, done, cpuMs: cpu / 1e6, mem, latencies, files: files.length };
}

const rows = [];
for (const project of projects) {
    const name = path.basename(project);
    const sourceBytes = fs.readdirSync(project).filter(f => f.endsWith('.sdsl')).reduce((n, f) => n + fs.statSync(path.join(project, f)).size, 0);
    const cacheDir = fs.mkdtempSync(path.join(process.env.TMPDIR ?? '/tmp', 'scratchpilerd-bench-'));
    const cold = await analysisRun(project, cacheDir);
    await cold.server.stop();
    fs.rmSync(cacheDir, { recursive: true, force: true });
    const loaded = await analysisRun(project, cacheDir, { interactive: true });
    await loaded.server.stop();
    const warm = await analysisRun(project, cacheDir, { interactive: true });
    const idleStartCpu = cpuNs(warm.pid), idleStartWake = wakeups(warm.pid);
    await sleep(idleSeconds * 1000);
    const idleCpuMs = (cpuNs(warm.pid) - idleStartCpu) / 1e6;
    const idleWakeups = wakeups(warm.pid) - idleStartWake;
    const idleMem = memory(warm.pid);
    await warm.server.stop();
    fs.rmSync(cacheDir, { recursive: true, force: true });
    const lat = [...loaded.latencies].sort((a, b) => a - b);
    const idleLat = [...warm.latencies].sort((a, b) => a - b);
    rows.push({
        project: name, files: cold.files, sourceKB: Math.round(sourceBytes / 1024),
        coldMs: Math.round(cold.done), coldCpuMs: Math.round(cold.cpuMs), coldCores: (cold.cpuMs / cold.done).toFixed(1), coldRssMB: cold.mem.rss.toFixed(1), coldPeakMB: cold.mem.peak.toFixed(1),
        warmMs: Math.round(warm.done), warmCpuMs: Math.round(warm.cpuMs), warmRssMB: warm.mem.rss.toFixed(1),
        idleCpuMsPerMin: (idleCpuMs * 60 / idleSeconds).toFixed(3), idleWakeupsPerMin: Math.round(idleWakeups * 60 / idleSeconds), idleRssMB: idleMem.rss.toFixed(1), threads: idleMem.threads,
        editDuringPassMs: loaded.latencies.edit?.toFixed(2), editIdleMs: warm.latencies.edit?.toFixed(2),
        duringPassP50: lat[Math.floor(lat.length / 2)].toFixed(2), duringPassP95: lat[Math.floor(lat.length * 0.95)].toFixed(2), duringPassMax: lat.at(-1).toFixed(2),
        idleP50: idleLat[Math.floor(idleLat.length / 2)].toFixed(2), idleMax: idleLat.at(-1).toFixed(2),
    });
    console.error(JSON.stringify(rows.at(-1)));
}
console.log(JSON.stringify(rows, null, 1));
