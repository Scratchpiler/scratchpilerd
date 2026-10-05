import fs from 'node:fs';
import path from 'node:path';
import { mulberry32, mutate, indexFromProject, spriteForFile, programs } from './programs.mjs';
import { toolsDir } from './js-bundle.mjs';

const MESSAGES = ['go', 'Go', 'tick', 'start', 'm1', 'm2', 'm3', 'ghost', 'init'];
const VARS = ['v1', 'v2', 'v3', 'score', 'log', '☁ cloud', '__heap'];

function snippets(rng, sprites) {
    const pick = list => list[Math.floor(rng() * list.length)];
    const msg = () => pick(MESSAGES);
    const v = () => pick(VARS);
    const target = () => pick([...sprites, 'Ghost', '_mouse_', '_edge_']);
    return [
        () => `on flag {\n    set [${v()}] to ${Math.floor(rng() * 3)}\n    broadcast("${msg()}")\n}`,
        () => `on flag {\n    say([${v()}])\n    set [${v()}] to [${v()}] + 1\n}`,
        () => `on receive "${msg()}" {\n    broadcastAndWait("${msg()}")\n}`,
        () => `on receive "${msg()}" {\n    change [${v()}] by 1\n    wait(1)\n    set [${v()}] to 2\n}`,
        () => `on key "space" {\n    set [${v()}] to ${Math.floor(rng() * 3)}\n}`,
        () => `on click {\n    set [${v()}] to "a"\n    say([${v()}])\n}`,
        () => `on clone {\n    say("clone")\n}`,
        () => `on flag {\n    createClone("${target()}")\n}`,
        () => `on flag {\n    clone()\n}`,
        () => `on flag {\n    say(attributeOf("${v()}", "${target()}"))\n    goTo("${target()}")\n    if touching("${target()}") {\n        stopAll()\n    }\n}`,
        () => `define helper${Math.floor(rng() * 2)}() {\n    broadcastAndWait("${msg()}")\n    set [${v()}] to 5\n}`,
        () => `on receive "${msg()}" {\n    helper${Math.floor(rng() * 2)}()\n}`,
        () => `scratchroutine r${Math.floor(rng() * 2)}() {\n    set [${v()}] to 1\n}`,
        () => `on flag {\n    launch r${Math.floor(rng() * 2)}()\n    await r${Math.floor(rng() * 2)}()\n}`,
        () => `on flag {\n    forever {\n        broadcast("${msg()}")\n    }\n}`,
        () => `on flag {\n    broadcast(join("m", [${v()}]))\n}`,
        () => `on backdrop "b1" {\n    set [${v()}] to 1\n}`,
        () => `on flag {\n    switchBackdrop("b1")\n    say(attributeOf("backdrop #", "_stage_"))\n    say(attributeOf("${v()}", "_stage_"))\n}`,
        () => `on flag {\n    __asm__ volatile( looks_say("hi"); )\n}`,
        () => `on flag {\n    stopOtherScripts()\n    deleteClone()\n}`,
        () => `define warpy() warp {\n    repeat 3 {\n        set [${v()}] to 1\n    }\n}\non flag {\n    warpy()\n    say([${v()}])\n}`,
        () => `on flag {\n    if [${v()}] = 1 {\n        set [${v()}] to 2\n    } else {\n        set [${v()}] to 3\n    }\n    say([${v()}])\n}`,
        () => `on flag {\n    match [${v()}] {\n        case 1 {\n            set [${v()}] to 4\n        }\n        default {\n            wait(1)\n        }\n    }\n}`,
        () => `on flag {\n    listAdd(1, [L1])\n    say([L1].item(1))\n}`,
        () => `define rec${Math.floor(rng() * 2)}()${rng() < 0.5 ? ' warp' : ''} {\n    set [${v()}] to [${v()}]\n    if [${v()}] > 0 {\n        change [${v()}] by -1\n        rec${Math.floor(rng() * 2)}()\n        mid()\n    }\n    repeat 2 {\n        say([${v()}])\n        rec${Math.floor(rng() * 2)}()\n    }\n}`,
        () => `define mid()${rng() < 0.5 ? ' warp' : ''} {\n    say([${v()}])\n    rec${Math.floor(rng() * 2)}()\n    set [${v()}] to ${Math.floor(rng() * 3)}\n    wait(${rng() < 0.3 ? 1 : 0})\n    rec${Math.floor(rng() * 2)}()\n}`,
        () => `on flag {\n    rec${Math.floor(rng() * 2)}()\n    mid()\n    set [${v()}] to 9\n}`,
        () => `on key "space" {\n    mid()\n    say([${v()}])\n    rec${Math.floor(rng() * 2)}()\n}`,
    ];
}

function syntheticProject(seed, fuzzTexts) {
    const rng = mulberry32(seed * 7919);
    const count = 1 + Math.floor(rng() * 4);
    const sprites = Array.from({ length: count }, (_, i) => `S${i}`);
    const make = snippets(rng, sprites);
    const files = {};
    for (const sprite of ['__stage__', ...sprites]) {
        const parts = [];
        const n = Math.floor(rng() * 6);
        for (let i = 0; i < n; i++) parts.push(make[Math.floor(rng() * make.length)]());
        if (rng() < 0.2) parts.push(fuzzTexts[Math.floor(rng() * fuzzTexts.length)]);
        let text = parts.join('\n\n');
        if (rng() < 0.15) text = mutate(text, rng, 1);
        files[sprite] = text;
    }
    if (rng() < 0.15) delete files[sprites[0]];
    const local = sprite => VARS.filter(() => rng() < 0.2).map(name => ({ name, id: `${sprite}-${name}`, type: 'variable' }));
    const index = {
        sprites: sprites.map(name => ({ name, costumes: ['c1'], sounds: [] })),
        stage: { backdrops: ['b1'], sounds: [] },
        globalVariables: [...VARS.filter(() => rng() < 0.8).map(name => ({ name, id: name, type: 'variable' })), { name: 'L1', id: 'L1', type: 'list' }],
        spriteVariables: Object.fromEntries(sprites.map(s => [s, local(s)])),
        customBlocks: Object.fromEntries(sprites.map(s => [s, []])),
        monitored: rng() < 0.5 ? [['__stage__', 'variable', 'v1']] : [],
    };
    return { name: `synthetic:${seed}`, files, index };
}

export function monitoredFromProject(project) {
    return (project.monitors ?? []).filter(m => m.visible && (m.params?.VARIABLE ?? m.params?.LIST))
        .map(m => [m.spriteName || '__stage__', m.opcode === 'data_listcontents' ? 'list' : 'variable', m.params.VARIABLE ?? m.params.LIST]);
}

export function projects({ count = 200, seed = 1 } = {}) {
    const out = [];
    const root = path.join(toolsDir, '..', 'target', 'corpus');
    for (const dir of fs.existsSync(root) ? fs.readdirSync(root) : []) {
        const project = JSON.parse(fs.readFileSync(path.join(root, dir, 'project.json'), 'utf8'));
        const index = { ...indexFromProject(project), monitored: monitoredFromProject(project) };
        const files = {};
        for (const f of fs.readdirSync(path.join(root, dir)).filter(f => f.endsWith('.sdsl'))) files[spriteForFile(f)] = fs.readFileSync(path.join(root, dir, f), 'utf8');
        out.push({ name: dir, files, index });
        const rng = mulberry32(seed + dir.length);
        for (let m = 0; m < 3; m++) {
            const mutated = { ...files };
            const victim = Object.keys(files)[Math.floor(rng() * Object.keys(files).length)];
            mutated[victim] = mutate(files[victim], rng, 1);
            out.push({ name: `${dir}:mutant:${m}`, files: mutated, index });
        }
    }
    const fuzzTexts = programs({ seeds: 30, mutationsPer: 0 }).filter(p => p.name.startsWith('fuzz:')).map(p => p.text);
    for (let s = 1; s <= count; s++) out.push(syntheticProject(s + seed * 100000, fuzzTexts));
    return out;
}
