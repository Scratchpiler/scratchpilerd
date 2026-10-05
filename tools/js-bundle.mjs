import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const toolsDir = path.dirname(fileURLToPath(import.meta.url));
export const scratchpilerDir = path.resolve(process.env.SCRATCHPILER_DIR ?? path.join(toolsDir, '..', '..', 'scratchpiler'));
export const srcDir = path.join(scratchpilerDir, 'src');
const { build } = createRequire(path.join(scratchpilerDir, 'package.json'))('esbuild');

const exposeInternals = {
    name: 'expose-internals',
    setup(b) {
        b.onLoad({ filter: /src[\\/]language\.js$/ }, args => {
            let text = fs.readFileSync(args.path, 'utf8');
            text = text.replace('    const KEYWORD_RE =', '    globalThis.__SIG_DB = SIG_DB; globalThis.__PEN_DOT_SIG = PEN_DOT_SIG;\n    const KEYWORD_RE =');
            text += '\nexport { MODIFIER_DOCS, KEY_NAMES, buildSuggestions, withCompletionBehavior, followUpCommand };\n';
            return { contents: text, loader: 'js' };
        });
    },
};

const stubMissingSlvm = {
    name: 'stub-missing-slvm',
    setup(b) {
        b.onResolve({ filter: /^slvm(\/.*)?$/ }, args => {
            try {
                createRequire(path.join(scratchpilerDir, 'package.json')).resolve(args.path);
                return undefined;
            } catch {
                return { path: args.path, namespace: 'slvm-stub' };
            }
        });
        b.onLoad({ filter: /.*/, namespace: 'slvm-stub' }, () => ({
            contents: 'module.exports = new Proxy({}, { get: (_, key) => key === "__esModule" ? false : () => { throw new Error("slvm is not installed"); } });',
            loader: 'js',
        }));
    },
};

function installBrowserStubs() {
    Object.assign(globalThis, {
        window: globalThis,
        document: { addEventListener() {}, querySelector: () => null, getElementById: () => null },
    });
    const store = new Map();
    Object.defineProperty(globalThis, 'localStorage', {
        value: { getItem: k => store.get(k) ?? null, setItem: (k, v) => store.set(k, String(v)), removeItem: k => store.delete(k) },
        configurable: true,
    });
}

export async function loadScratchpiler(extraExports = '') {
    installBrowserStubs();
    const { outputFiles } = await build({
        stdin: {
            contents: `
                export * as language from './language.js';
                export * as compiler from './compiler.js';
                export * as analyzer from './analyzer.js';
                export * as constants from './constants.js';
                export * as asm from './asm-opcodes.js';
                export * as scratchIndex from './scratch-index.js';
                export * as completionContext from './completion-context.js';
                export * as projectAnalysis from './project-analysis.js';
                export * as projectFacts from './project-facts.js';
                export * as broadcastHints from './broadcast-hints.js';
                export * as format from './format.js';
                export * as headers from './headers.js';
                export * as preprocess from './preprocess.js';
                export * as variables from './variables.js';
                ${extraExports}`,
            resolveDir: srcDir,
        },
        bundle: true, write: false, platform: 'node', format: 'esm', logLevel: 'silent',
        loader: { '.css': 'text', '.html': 'text' },
        plugins: [exposeInternals, stubMissingSlvm],
        nodePaths: [path.join(scratchpilerDir, 'node_modules')],
    });
    const file = path.join(fs.mkdtempSync(path.join(process.env.TMPDIR ?? '/tmp', 'scratchpilerd-bundle-')), 'bundle.mjs');
    fs.writeFileSync(file, outputFiles[0].text);
    return import(pathToFileURL(file).href);
}

const noop = () => ({ dispose() {} });

export function fakeMonaco(onRegister = () => {}) {
    const kinds = new Proxy({}, { get: (_, k) => k });
    const languages = new Proxy({
        CompletionItemKind: kinds,
        CompletionItemInsertTextRule: { InsertAsSnippet: 4 },
        FoldingRangeKind: { Region: 'region' },
        DocumentHighlightKind: { Read: 'read', Write: 'write', Text: 'text' },
    }, {
        get: (target, k) => k in target ? target[k] : (k.startsWith('register') || k.startsWith('set')
            ? (...args) => { onRegister(k, args); return noop(); }
            : noop),
    });
    class Range {
        constructor(a, b, c, d) { Object.assign(this, { startLineNumber: a, startColumn: b, endLineNumber: c, endColumn: d }); }
        static isIRange(r) { return r && 'startLineNumber' in r; }
    }
    return { languages, editor: new Proxy({}, { get: () => noop }), MarkerSeverity: { Error: 8, Warning: 4, Info: 2, Hint: 1 }, MarkerTag: { Unnecessary: 1 }, Uri: {}, Range };
}
