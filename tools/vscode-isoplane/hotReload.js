'use strict';
const path = require('node:path');
const vscode = require('vscode');

function register(context) {
    const sessions = new Map();
    const output = vscode.window.createOutputChannel('Isoplane Hot Reload');
    const key = file => process.platform === 'win32' ? path.resolve(file).toLowerCase() : path.resolve(file);

    async function apply(state, manual = false) {
        if (state.applying) return state.applying;
        if (!state.paused) {
            if (manual) throw new Error('Pause the application before applying code changes. Saved edits will be applied at the next stop.');
            return;
        }
        if (state.pending.size === 0) return;
        const edits = new Map(state.pending);
        state.pending.clear();
        const generation = state.generation;
        state.applying = sendEdits(state, edits);
        let success;
        try { success = await state.applying; }
        finally { state.applying = undefined; }
        if (state.generation !== generation) return;
        if (!success) {
            for (const [file, edit] of edits) if (!state.pending.has(file)) state.pending.set(file, edit);
        } else if (state.pending.size !== 0) {
            await apply(state);
        }
    }

    async function sendEdits(state, edits) {
        try {
            const reply = await vscode.window.withProgress({ location: vscode.ProgressLocation.Window, title: 'Applying Isoplane code changes' },
                () => state.session.customRequest('isoplane/hotReload', { sources: [...edits.values()] }));
            const success = !reply.restartRequired && reply.diagnostics.length === 0;
            if (!success) {
                for (const diagnostic of reply.diagnostics) output.appendLine(diagnostic);
                if (reply.restartRequired) output.appendLine('These edits require restarting the application.');
                output.show(true);
            }
            return success;
        } catch (error) {
            output.appendLine('Could not apply code changes: ' + error.message);
            output.show(true);
            return false;
        }
    }

    context.subscriptions.push(output,
        vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
            createDebugAdapterTracker(session) {
                const state = { session, documents: new Map(), pending: new Map(), paused: false, applying: undefined, generation: 0 };
                sessions.set(session.id, state);
                const resumes = new Set();
                return {
                    onWillReceiveMessage(message) {
                        if (['continue', 'next', 'stepIn', 'stepOut', 'goto', 'restartFrame'].includes(message.command)) {
                            resumes.add(message.seq);
                            state.paused = false;
                        }
                    },
                    onDidSendMessage(message) {
                        if (message.type === 'response' && resumes.delete(message.request_seq) && !message.success) state.paused = true;
                        if (message.type !== 'event') return;
                        if (message.event === 'isoplane/hotReloadReady') {
                            state.generation++;
                            state.documents = new Map(message.body.documents.map(file => [key(file), file]));
                            state.pending.clear();
                            state.paused = false;
                        } else if (message.event === 'stopped') {
                            state.paused = true;
                            void apply(state);
                        } else if (['continued', 'exited', 'terminated'].includes(message.event)) {
                            state.paused = false;
                        }
                    },
                    onExit() { state.generation++; state.paused = false; sessions.delete(session.id); }
                };
            }
        }),
        vscode.workspace.onDidSaveTextDocument(document => {
            if (document.uri.scheme !== 'file') return;
            const file = key(document.uri.fsPath);
            for (const state of sessions.values()) {
                const source = state.documents.get(file);
                if (!source) continue;
                state.pending.set(file, { path: source, text: document.getText() });
                void apply(state);
            }
        }),
        vscode.commands.registerCommand('isoplane.applyCodeChanges', async () => {
            const state = sessions.get(vscode.debug.activeDebugSession?.id);
            if (!state || state.documents.size === 0) throw new Error('Select an Isoplane source debugging session to apply code changes.');
            await vscode.workspace.saveAll(false);
            await apply(state, true);
        })
    );
}

module.exports = { register };
