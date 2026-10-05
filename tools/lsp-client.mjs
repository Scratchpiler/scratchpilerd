import { spawn } from 'node:child_process';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { toolsDir } from './js-bundle.mjs';

export function startServer(args, { binary = path.join(toolsDir, '..', 'target', 'release', 'scratchpilerd'), env = {} } = {}) {
    const child = spawn(binary, args, { stdio: ['pipe', 'pipe', 'inherit'], env: { ...process.env, ...env } });
    let buffer = Buffer.alloc(0);
    let nextId = 1;
    const pending = new Map();
    const notifications = [];
    const waiters = [];
    child.stdout.on('data', chunk => {
        buffer = Buffer.concat([buffer, chunk]);
        for (;;) {
            const headerEnd = buffer.indexOf('\r\n\r\n');
            if (headerEnd < 0) return;
            const length = Number(/Content-Length: (\d+)/i.exec(buffer.slice(0, headerEnd).toString())[1]);
            if (buffer.length < headerEnd + 4 + length) return;
            const message = JSON.parse(buffer.slice(headerEnd + 4, headerEnd + 4 + length).toString());
            buffer = buffer.slice(headerEnd + 4 + length);
            if (message.id !== undefined && !message.method && pending.has(message.id)) {
                pending.get(message.id)(message);
                pending.delete(message.id);
            } else if (message.method) {
                if (message.id !== undefined) send({ jsonrpc: '2.0', id: message.id, result: null });
                notifications.push(message);
                for (const w of [...waiters]) if (w.test(message)) { waiters.splice(waiters.indexOf(w), 1); w.resolve(message); }
            }
        }
    });
    function send(message) {
        const body = Buffer.from(JSON.stringify(message));
        child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
        child.stdin.write(body);
    }
    return {
        child, notifications,
        request(method, params) {
            const id = nextId++;
            send({ jsonrpc: '2.0', id, method, params });
            return new Promise(resolve => pending.set(id, resolve));
        },
        notify(method, params) { send({ jsonrpc: '2.0', method, params }); },
        waitFor(test, timeout = 10000) {
            const found = notifications.find(test);
            if (found) return Promise.resolve(found);
            return new Promise((resolve, reject) => {
                const w = { test, resolve };
                waiters.push(w);
                setTimeout(() => reject(new Error('timeout waiting for notification')), timeout);
            });
        },
        async stop() {
            await this.request('shutdown', null);
            this.notify('exit', null);
            return new Promise(resolve => child.on('exit', resolve));
        },
    };
}

export const uriOf = file => pathToFileURL(file).href;
