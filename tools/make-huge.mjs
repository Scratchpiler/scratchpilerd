import fs from 'node:fs';
import path from 'node:path';
import { toolsDir } from './js-bundle.mjs';

const copies = Number(process.argv[2] ?? 10);
const source = path.join(toolsDir, '..', 'target', 'corpus', 'Paper-Minecraft-v11.7-Minecraft-2D-');
const out = path.join(toolsDir, '..', 'target', 'bench', `paper-x${copies}`);
fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(out, { recursive: true });
const project = JSON.parse(fs.readFileSync(path.join(source, 'project.json'), 'utf8'));
const stage = project.targets.find(t => t.isStage);
const sprites = project.targets.filter(t => !t.isStage);
const targets = [stage];
for (let c = 0; c < copies; c++) {
    for (const sprite of sprites) {
        const name = c === 0 ? sprite.name : `${sprite.name} ${c}`;
        targets.push({ ...sprite, name, blocks: {} });
        const file = sprite.name.replace(/[%<>:"/\\|?*]/g, ch => '%' + ch.charCodeAt(0).toString(16).toUpperCase());
        const text = fs.readFileSync(path.join(source, `${file}.sdsl`), 'utf8');
        fs.writeFileSync(path.join(out, `${name.replace(/[%<>:"/\\|?*]/g, ch => '%' + ch.charCodeAt(0).toString(16).toUpperCase())}.sdsl`), text);
    }
}
fs.copyFileSync(path.join(source, 'Stage.sdsl'), path.join(out, 'Stage.sdsl'));
fs.writeFileSync(path.join(out, 'project.json'), JSON.stringify({ ...project, targets: targets.map(t => ({ ...t, blocks: t.isStage ? {} : t.blocks })) }));
const bytes = fs.readdirSync(out).filter(f => f.endsWith('.sdsl')).reduce((n, f) => n + fs.statSync(path.join(out, f)).size, 0);
console.log(`${out}: ${targets.length} targets, ${(bytes / 1024 / 1024).toFixed(1)} MB of source`);
