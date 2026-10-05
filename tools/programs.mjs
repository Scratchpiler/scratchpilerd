import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { scratchpilerDir, toolsDir } from './js-bundle.mjs';

const { generateProgram } = await import(pathToFileURL(path.join(scratchpilerDir, 'tests', 'fuzz', 'generate.js')));

export function mulberry32(seed) {
    let a = seed >>> 0;
    return () => {
        a = (a + 0x6D2B79F5) >>> 0;
        let t = a;
        t = Math.imul(t ^ (t >>> 15), t | 1);
        t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
}

const NOISE = ['(', ')', '{', '}', '[', ']', '"', ',', '.', '=', '+=', '-=', '++', '?', ':', ';', '#', '&', '*', '!', '\n', ' ', '//',
    'define', 'on', 'if', 'else', 'repeat', 'until', 'for', 'from', 'to', 'pyfor', 'in', 'match', 'case', 'default', 'do', 'while',
    'scratchroutine', 'launch', 'await', 'cancel', 'return', 'returns', 'warp', 'struct', 'enum', '__asm__', 'volatile', 'unsafe',
    'pen', 'set', 'change', 'move', 'say', 'broadcast', 'flag', 'receive', 'key', 'timer', 'not', 'and', 'or', 'mod', '0', '7', '1.5e3',
    '"go"', '"{1}"', '"{[a]}"', '[a]', '[l]', '[a.x]', 'x', 'myBlock', '#ff00aa', '雪', '🐈', '\\u0041', '"\\"', 'nounroll', 'max', 'sort'];

export function mutate(source, rng, edits = 1 + Math.floor(rng() * 4)) {
    let text = source;
    for (let i = 0; i < edits; i++) {
        const at = Math.floor(rng() * (text.length + 1));
        const roll = rng();
        if (roll < 0.4) text = text.slice(0, at) + text.slice(at + 1 + Math.floor(rng() * 6));
        else if (roll < 0.8) text = text.slice(0, at) + NOISE[Math.floor(rng() * NOISE.length)] + text.slice(at);
        else {
            const from = Math.floor(rng() * text.length);
            text = text.slice(0, at) + text.slice(from, from + Math.floor(rng() * 30)) + text.slice(at);
        }
    }
    return text;
}

export function indexFromProject(project) {
    const index = { sprites: [], stage: { backdrops: [], sounds: [] }, globalVariables: [], spriteVariables: {}, customBlocks: {} };
    for (const target of project.targets) {
        const costumes = (target.costumes ?? []).map(c => c.name);
        const sounds = (target.sounds ?? []).map(s => s.name);
        const vars = [
            ...Object.entries(target.variables ?? {}).map(([id, [name]]) => ({ name, id, type: 'variable' })),
            ...Object.entries(target.lists ?? {}).map(([id, [name]]) => ({ name, id, type: 'list' })),
        ];
        if (target.isStage) {
            index.stage = { backdrops: costumes, sounds };
            index.globalVariables = vars;
        } else {
            index.sprites.push({ name: target.name, costumes, sounds });
            index.spriteVariables[target.name] = vars;
            index.customBlocks[target.name] = Object.values(target.blocks ?? {})
                .filter(b => b && b.opcode === 'procedures_prototype').map(b => b.mutation?.proccode).filter(Boolean);
        }
    }
    return index;
}

export const spriteForFile = file => {
    const base = path.basename(file).replace(/\.(sdsl|h)$/, '');
    return base === 'Stage' ? '__stage__' : base.replace(/%([0-9A-F]{2})/g, (_, hex) => String.fromCharCode(parseInt(hex, 16)));
};

function hashName(name) {
    let h = 2166136261;
    for (const c of name) h = Math.imul(h ^ c.charCodeAt(0), 16777619) >>> 0;
    return h;
}

export function syntheticIndex(text) {
    const names = [...new Set([...text.matchAll(/\[([^\]\n]*)\]/g)].map(m => m[1].trim()))];
    const index = { sprites: [{ name: 'Sprite1', costumes: ['costume1'], sounds: ['pop'] }, { name: 'Other', costumes: [], sounds: [] }],
        stage: { backdrops: ['backdrop1'], sounds: [] }, globalVariables: [], spriteVariables: { Sprite1: [], Other: [] }, customBlocks: { Sprite1: [], Other: [] } };
    for (const name of names) {
        const h = hashName(name);
        const bucket = h % 5;
        const target = (h >>> 8) % 2 ? index.globalVariables : index.spriteVariables.Sprite1;
        if (bucket === 0 || bucket === 3) target.push({ name, id: `v${h}`, type: 'variable' });
        if (bucket === 1 || bucket === 3) target.push({ name, id: `l${h}`, type: 'list' });
    }
    if (!index.globalVariables.length && !index.spriteVariables.Sprite1.length && hashName(text) % 4) index.globalVariables.push({ name: 'zz', id: 'zz', type: 'variable' });
    return index;
}

export function corpusFiles(root = path.join(toolsDir, '..', 'target', 'corpus')) {
    if (!fs.existsSync(root)) return [];
    return fs.readdirSync(root).flatMap(project => fs.readdirSync(path.join(root, project))
        .filter(f => f.endsWith('.sdsl') || f.endsWith('.h'))
        .map(f => path.join(root, project, f)));
}

const FEATURE_SETS = [
    undefined,
    { listSugar: true, conditionCalls: true, recursion: true, continueInFor: true, breakInFor: true, pointers: true, weirdLiterals: true, warpLoops: true },
];

export function programs({ seeds = 300, mutationsPer = 3, seed = 1 } = {}) {
    const rng = mulberry32(seed);
    const out = [];
    const extraDirs = [path.join(scratchpilerDir, 'examples'), path.join(scratchpilerDir, 'run', 'catos', 'src'), path.join(scratchpilerDir, 'run', 'catos', 'src', 'include')];
    const files = [...corpusFiles(), ...extraDirs.filter(fs.existsSync).flatMap(d => fs.readdirSync(d).filter(f => /\.(sdsl|h|txt)$/.test(f)).map(f => path.join(d, f)))];
    const projectIndex = new Map();
    const indexFor = file => {
        const projectFile = path.join(path.dirname(file), 'project.json');
        if (!fs.existsSync(projectFile)) return null;
        if (!projectIndex.has(projectFile)) projectIndex.set(projectFile, indexFromProject(JSON.parse(fs.readFileSync(projectFile, 'utf8'))));
        return projectIndex.get(projectFile);
    };
    for (const file of files) {
        const text = fs.readFileSync(file, 'utf8');
        const index = indexFor(file);
        out.push({ name: file, text, index: index ?? syntheticIndex(text), sprite: index ? spriteForFile(file) : 'Sprite1' });
    }
    for (let s = 1; s <= seeds; s++) {
        for (const features of FEATURE_SETS) {
            const text = generateProgram(s, features);
            out.push({ name: `fuzz:${s}:${features ? 'all' : 'default'}`, text, index: syntheticIndex(text), sprite: 'Sprite1' });
            for (let m = 0; m < mutationsPer; m++) {
                const mutant = mutate(text, rng);
                out.push({ name: `mutant:${s}:${m}`, text: mutant, index: syntheticIndex(mutant), sprite: 'Sprite1' });
            }
        }
    }
    for (const file of files.slice(0, 60)) {
        const text = fs.readFileSync(file, 'utf8');
        const lines = text.split('\n');
        const index = indexFor(file);
        for (let m = 0; m < mutationsPer; m++) {
            const start = Math.floor(rng() * lines.length);
            const mutant = mutate(lines.slice(start, start + 40).join('\n'), rng);
            out.push({ name: `${file}:mutant:${m}`, text: mutant, index: index ?? syntheticIndex(mutant), sprite: index ? spriteForFile(file) : 'Sprite1' });
        }
    }
    return out;
}
