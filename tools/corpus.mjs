import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { scratchpilerDir, toolsDir } from './js-bundle.mjs';
import { readZipEntry } from './sb3.mjs';

const { decompile } = await import(pathToFileURL(path.join(scratchpilerDir, 'src', 'decompiler.js')));
const { makeMockVM } = await import(pathToFileURL(path.join(scratchpilerDir, 'tests', 'mock-vm.js')));

export const spriteFileName = name => name.replace(/[%<>:"/\\|?*\x00-\x1f]/g, c => '%' + c.charCodeAt(0).toString(16).padStart(2, '0').toUpperCase()) + '.sdsl';

function loadVM(project) {
    const vm = makeMockVM({ sprites: project.targets.filter(t => !t.isStage).map(t => t.name) });
    project.targets.forEach((source, index) => {
        const target = vm.runtime.targets[index];
        target.sprite.costumes = source.costumes;
        target.sprite.sounds = source.sounds;
        target.comments = source.comments ?? {};
        for (const [id, [name, value]] of Object.entries(source.variables ?? {})) {
            target.createVariable(id, name, '');
            target.variables[id].value = value;
        }
        for (const [id, [name, value]] of Object.entries(source.lists ?? {})) {
            target.createVariable(id, name, 'list');
            target.variables[id].value = value;
        }
        for (const [id, name] of Object.entries(source.broadcasts ?? {})) target.createVariable(id, name, 'broadcast_msg');
        target.blocks._blocks = source.blocks;
    });
    return vm;
}

export function writeCorpusProject(projectJson, outDir) {
    const project = JSON.parse(projectJson);
    const vm = loadVM(project);
    fs.mkdirSync(outDir, { recursive: true });
    fs.writeFileSync(path.join(outDir, 'project.json'), projectJson);
    let failures = 0;
    for (const target of vm.runtime.targets) {
        const name = target.isStage ? '__stage__' : target.sprite.name;
        let source;
        try {
            source = decompile(vm, name);
        } catch (error) {
            failures++;
            source = `// decompile failed: ${String(error.message).split('\n')[0]}\n`;
        }
        fs.writeFileSync(path.join(outDir, target.isStage ? 'Stage.sdsl' : spriteFileName(name)), source);
    }
    return { targets: vm.runtime.targets.length, failures };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
    const outRoot = path.resolve(process.argv[2] ?? path.join(toolsDir, '..', 'target', 'corpus'));
    const inputs = process.argv.slice(3);
    const sb3s = inputs.length ? inputs : fs.readdirSync(path.join(scratchpilerDir, '..', 'test-projects')).filter(f => f.endsWith('.sb3')).map(f => path.join(scratchpilerDir, '..', 'test-projects', f));
    for (const sb3 of sb3s) {
        const json = readZipEntry(sb3, 'project.json');
        const name = path.basename(sb3, '.sb3').replace(/[^\w.-]+/g, '-');
        const result = writeCorpusProject(json.toString('utf8'), path.join(outRoot, name));
        console.log(`${name}: ${result.targets} targets, ${result.failures} decompile failures`);
    }
}
