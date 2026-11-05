import { describe, expect, it, vi } from 'vitest';
import { EditorOwnership, type EditorContext, type EditorRead } from './editor-ownership';
const context: EditorContext = { profile_id:'a', note_id:'n', session_generation:1, editor_id:'e', loaded_revision:'r1' };
function setup() {
 const calls: string[] = [];
 let disk = 'before';
 const transport = {
  begin: async () => { calls.push('begin'); },
  save: async (ctx: EditorContext, content: string) => { if (ctx.loaded_revision !== 'r1') throw Error('REVISION_CONFLICT'); disk=content; calls.push('save'); return {context:{...ctx,loaded_revision:'r2'},note:{content,meta:{id:'n'}}}; },
  ack: async (_ctx: EditorContext, _id: string, dirty: boolean) => { calls.push(dirty?'busy':'ack'); },
  read: async (ctx: EditorContext) => ({context:{...ctx,loaded_revision:'r2'}, note:{content:disk,meta:{id:'n'}}}),
  end: async () => { calls.push('end'); },
 };
 const view = {setEditable:vi.fn(),load:vi.fn(),conflict:vi.fn(),persist:vi.fn()};
 const owner = new EditorOwnership(context, 'before', transport, view);
 return {owner,calls,transport,view};
}
function deferredRead() {
 let resolve!:(data:EditorRead)=>void;
 let reject!:(error:Error)=>void;
 const promise=new Promise<EditorRead>((done,fail)=>{resolve=done;reject=fail;});
 return {promise,resolve,reject};
}
const read=(content:string,revision:string):EditorRead=>({context:{...context,loaded_revision:revision},note:{content,meta:{id:'n'}}});
describe('editor ownership timing', () => {
 it('dirty_editor_blocks_mcp before debounce or IPC resolves', async () => { const {owner,calls}=setup(); owner.change('draft'); expect(owner.dirty).toBe(true); await owner.prepare('p'); expect(calls).toContain('busy'); });
 it('clean_editor_pauses_then_reloads', async () => { const {owner,calls}=setup(); await owner.prepare('p'); expect(owner.paused).toBe(true); expect(calls).toContain('ack'); await owner.resume('p',true); expect(owner.paused).toBe(false); expect(owner.context.loaded_revision).toBe('r2'); });
 it('late_keystroke_preserves_draft', async () => {const {owner}=setup(); await owner.prepare('p'); owner.change('late draft'); await owner.resume('p',true); expect(owner.content).toBe('late draft'); expect(owner.dirty).toBe(true); expect(owner.conflicted).toBe(true);});
 it('failed_autosave_keeps_dirty_buffer', async () => { const {owner,transport}=setup(); transport.save=async()=>{throw Error('REVISION_CONFLICT')}; owner.change('draft'); await expect(owner.flush()).rejects.toThrow(); expect(owner.content).toBe('draft'); expect(owner.dirty).toBe(true); expect(owner.conflicted).toBe(true); });
 it('save_does_not_clear_newer_keystrokes', async () => { const {owner,transport}=setup(); let release!:()=>void; transport.save=async(ctx,content)=>{await new Promise<void>(r=>release=r); return {context:{...ctx,loaded_revision:'r2'},note:{content,meta:{id:'n'}}}}; owner.change('first'); const saving=owner.flush(); await vi.waitFor(()=>expect(release).toBeTypeOf('function')); owner.change('second'); release(); await saving; expect(owner.dirty).toBe(true); expect(owner.content).toBe('second'); });
 it('late_save_after_profile_switch_keeps_original_context', async()=>{const {owner,transport}=setup(); transport.begin=async(ctx?:EditorContext)=>{expect(ctx?.profile_id).toBe('a'); throw Error('EDITOR_SESSION_EXPIRED')}; owner.change('draft'); await expect(owner.flush()).rejects.toThrow(); expect(owner.context.profile_id).toBe('a'); expect(owner.dirty).toBe(true);});
 it('restored_draft_never_overwrites_a_newer_revision',async()=>{const {owner}=setup();owner.restoreDraft('recovered draft','old');expect(owner.content).toBe('recovered draft');expect(owner.dirty).toBe(true);await expect(owner.flush()).rejects.toThrow('REVISION_CONFLICT');});
 it('delayed_read_never_reloads_a_dirty_buffer',async()=>{const {owner,transport}=setup();let release!:()=>void;transport.read=async(ctx)=>{await new Promise<void>(r=>release=r);return {context:ctx,note:{content:'remote',meta:{id:'n'}}}};const reading=owner.refresh();await vi.waitFor(()=>expect(release).toBeTypeOf('function'));owner.change('draft');release();await reading;expect(owner.content).toBe('draft');expect(owner.conflicted).toBe(true);});
 it('rapid_tab_switches_release_the_same_owner_only_once',async()=>{const {owner,calls}=setup();await Promise.all([owner.close(),owner.close()]);expect(calls.filter(call=>call==='end')).toHaveLength(1);});
 it('late_prepare_after_timeout_reloads_and_resumes_a_clean_editor',async()=>{const {owner,transport}=setup();transport.ack=async()=>{throw Error('EDITOR_BUSY')};await owner.prepare('expired');expect(owner.paused).toBe(false);expect(owner.context.loaded_revision).toBe('r2');expect(owner.conflicted).toBe(false);});

 it.each(['resume','refresh'] as const)('serializes a pending %s body/context before a later reservation resumes',async source=>{
  const {owner,transport,view}=setup();const older=deferredRead();const newer=deferredRead();let reads=0;
  transport.read=()=>++reads===1?older.promise:newer.promise;
  if(source==='resume')await owner.prepare('first');
  const first=source==='resume'?owner.resume('first',true):owner.refresh();
  const preparingSecond=owner.prepare('second');const second=owner.resume('second',true);
  newer.resolve(read('Newest','r3'));
  older.resolve(read('Older','r2'));await first;await preparingSecond;await second;
  expect(owner.content).toBe('Newest');expect(owner.context.loaded_revision).toBe('r3');
  expect(view.load.mock.calls).toEqual([['Older'],['Newest']]);expect(owner.paused).toBe(false);expect(owner.conflicted).toBe(false);
 });
 it.each(['resume','refresh'] as const)('does not unpause a newer reservation when an older %s completes',async source=>{
  const {owner,transport,view}=setup();const older=deferredRead();transport.read=()=>older.promise;
  if(source==='resume')await owner.prepare('first');
  const first=source==='resume'?owner.resume('first',true):owner.refresh();
  const preparingSecond=owner.prepare('second');older.resolve(read('Older','r2'));await first;await preparingSecond;
  expect(owner.paused).toBe(true);expect(owner.content).toBe('Older');expect(owner.context.loaded_revision).toBe('r2');
  expect(view.setEditable).toHaveBeenLastCalledWith(false);expect(view.load.mock.calls).toEqual([['Older']]);
  await owner.resume('second',false);expect(owner.paused).toBe(false);
 });
 it.each(['resume','refresh'] as const)('retains a current %s error and rejects a queued prepare without loading stale content',async source=>{
  const {owner,transport,view,calls}=setup();const older=deferredRead();
  transport.read=()=>older.promise;
  if(source==='resume')await owner.prepare('first');
  const first=source==='resume'?owner.resume('first',true):owner.refresh();
  const preparingSecond=owner.prepare('second');
  await Promise.resolve();older.reject(Error('current read failed'));await first;await preparingSecond;
  expect(owner.conflicted).toBe(true);expect(view.conflict).toHaveBeenCalledTimes(1);expect(calls).toContain('busy');
  expect(owner.content).toBe('before');expect(owner.paused).toBe(true);expect(view.setEditable).toHaveBeenLastCalledWith(false);
 });
 it('retains an acknowledgement failure before rejecting a queued later prepare',async()=>{
  const {owner,transport,view}=setup();let reject!:(error:Error)=>void;
  transport.ack=async(_ctx,id)=>{if(id==='first')await new Promise<void>((_done,fail)=>{reject=fail;});};
  const first=owner.prepare('first');const second=owner.prepare('second');await Promise.resolve();reject(Error('current acknowledgement failed'));await first;await second;
  expect(owner.conflicted).toBe(true);expect(view.conflict).toHaveBeenCalledTimes(1);expect(owner.paused).toBe(true);
 });
 it('keeps a dirty draft when the current resume read completes after a keystroke',async()=>{
  const {owner,transport,view}=setup();const pending=deferredRead();transport.read=()=>pending.promise;
  await owner.prepare('current');const reading=owner.resume('current',true);await Promise.resolve();owner.change('draft');pending.resolve(read('remote','r2'));await reading;
  expect(owner.content).toBe('draft');expect(owner.dirty).toBe(true);expect(owner.conflicted).toBe(true);expect(owner.paused).toBe(true);
  expect(view.load).not.toHaveBeenCalled();
 });
 it.each(['resume','refresh'] as const)('acknowledges a newer prepare with the revision returned by an in-flight %s',async source=>{
  const {owner,transport,view}=setup();const firstReply=deferredRead();let backendRevision='r1';let reads=0;
  const acknowledgements:{id:string;revision:string;dirty:boolean}[]=[];
  transport.ack=async(ctx,id,dirty)=>{
   if(ctx.loaded_revision!==backendRevision)throw Error('REVISION_CONFLICT');
   acknowledgements.push({id,revision:ctx.loaded_revision,dirty});
  };
  transport.read=async ctx=>{
   if(ctx.loaded_revision!==backendRevision)throw Error('REVISION_CONFLICT');
   // The real native reloaded() updates its entry BEFORE the IPC reply arrives.
   backendRevision=++reads===1?'r2':'r3';
   return reads===1?firstReply.promise:read('Newest','r3');
  };
  if(source==='resume')await owner.prepare('first');
  const first=source==='resume'?owner.resume('first',true):owner.refresh();
  await Promise.resolve();
  const secondPrepare=owner.prepare('second');
  await Promise.resolve();
  firstReply.resolve(read('First committed','r2'));await first;await secondPrepare;
  expect(owner.content).toBe('First committed');expect(owner.context.loaded_revision).toBe('r2');
  expect(acknowledgements[acknowledgements.length-1]).toEqual({id:'second',revision:'r2',dirty:false});
  expect(owner.conflicted).toBe(false);expect(owner.paused).toBe(true);expect(view.setEditable).toHaveBeenLastCalledWith(false);
  await owner.resume('second',true);
  expect(owner.content).toBe('Newest');expect(owner.context.loaded_revision).toBe('r3');expect(owner.conflicted).toBe(false);expect(owner.paused).toBe(false);
 });
 it('rejects a dirty prepare immediately while a clean reload reply is still pending',async()=>{
  const {owner,transport,calls}=setup();const pending=deferredRead();transport.read=()=>pending.promise;
  await owner.prepare('first');const reading=owner.resume('first',true);await Promise.resolve();
  owner.change('draft');await owner.prepare('second');
  expect(calls[calls.length-1]).toBe('busy');expect(owner.content).toBe('draft');expect(owner.dirty).toBe(true);
  pending.resolve(read('remote','r2'));await reading;
  expect(owner.content).toBe('draft');expect(owner.conflicted).toBe(true);
 });
});
