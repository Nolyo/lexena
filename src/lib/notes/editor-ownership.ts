export interface EditorContext { readonly profile_id:string; readonly note_id:string; readonly session_generation:number; readonly editor_id:string; readonly loaded_revision:string }
export interface EditorRead { context:EditorContext; note:{content:string;meta:{id:string;[key:string]:unknown}} }
export interface EditorTransport {
 begin(context:EditorContext):Promise<unknown>;
 save(context:EditorContext,content:string):Promise<EditorRead>;
 ack(context:EditorContext,prepareId:string,dirty:boolean):Promise<unknown>;
 read(context:EditorContext):Promise<EditorRead>;
 end(context:EditorContext):Promise<unknown>;
}
interface EditorView {setEditable(value:boolean):void;load(content:string):void;conflict(error:unknown):void;persist(context:EditorContext,content:string|null):void;saved?(data:EditorRead):void}
/** Renderer-owned state is authoritative for pending keystrokes, even before IPC arrives. */
export class EditorOwnership {
 dirty=false; paused=false; conflicted=false;
 private epoch=0;
 private readGeneration=0;
 private prepareId:string|null=null;
 private lifecycle:Promise<unknown>=Promise.resolve();
 private pendingPreparations=0;
 private chain:Promise<unknown>=Promise.resolve();
 private failure:unknown=null;
 private closing:Promise<void>|null=null;
 constructor(public context:EditorContext,public content:string,private transport:EditorTransport,private view:EditorView){}
 restoreDraft(content:string,revision:string){
  // An earlier non-document update could retain the already-persisted HTML.
  // Discard only that byte-identical duplicate, never a different recovery draft.
  if(!this.dirty&&content===this.content){this.view.persist(this.context,null);return;}
  this.content=content;this.dirty=true;this.epoch++;if(revision!==this.context.loaded_revision){this.fail(Error('REVISION_CONFLICT'));}else{this.change(content);}
 }
 private fail(error:unknown) {this.failure=error;this.conflicted=true;this.view.conflict(error);}
 private isCurrentRead(generation:number,prepareId:string|null) {
  return generation===this.readGeneration&&prepareId===this.prepareId;
 }
 private enqueueLifecycle(work:()=>Promise<void>):Promise<void> {
  const pending=this.lifecycle.then(work);
  this.lifecycle=pending.catch(()=>{});
  return pending;
 }
 private unpause() {
  // A queued prepare must stay non-editable while a previous read installs
  // the backend's already-advanced revision, before the next acknowledgement.
  if(this.pendingPreparations)return;
  this.paused=false;this.view.setEditable(true);
 }
 change(content:string) {
  this.content=content;this.dirty=true;this.epoch++;
  this.view.persist(this.context,content);
  if(this.paused) {this.fail(Error('EDITOR_BUSY')); return;}
  // Queue the barrier immediately, before the caller starts its debounce timer.
  this.chain=this.chain.then(()=>this.transport.begin(this.context)).catch(error=>this.fail(error));
 }
 prepare(id:string):Promise<void> {
  // A dirty renderer rejects immediately, without waiting behind clean reads.
  if(this.dirty||this.conflicted)return this.transport.ack(this.context,id,true).then(()=>{});
  this.pendingPreparations++;this.paused=true;this.view.setEditable(false);
  return this.enqueueLifecycle(async()=>{this.pendingPreparations--;await this.prepareNow(id);});
 }
 private async prepareNow(id:string) {
  if(this.dirty||this.conflicted) {await this.transport.ack(this.context,id,true);return;}
  const generation=++this.readGeneration;
  this.paused=true;this.prepareId=id;this.view.setEditable(false);
  try {await this.transport.ack(this.context,id,false);} catch(error) {
   if(!this.isCurrentRead(generation,id))return;
   // A prepare event may arrive after its reservation timed out/released.
   // No dirty buffer is reloaded; a clean editor refreshes before resuming.
   if(String(error).includes('EDITOR_BUSY')&&!this.dirty)await this.resumeNow(id,true);
   else this.fail(error);
  }
 }
 resume(id:string,committed:boolean):Promise<void> {
  return this.enqueueLifecycle(()=>this.resumeNow(id,committed));
 }
 private async resumeNow(id:string,committed:boolean) {
  if(this.prepareId!==id)return;
  if(this.dirty) {this.fail(Error('REVISION_CONFLICT'));return;}
  const generation=++this.readGeneration;const epoch=this.epoch;
  try {
   if(committed) {
    const data=await this.transport.read(this.context);
    // A newer prepare/read owns both the body/context and the pause state.
    if(!this.isCurrentRead(generation,id))return;
    // A transaction can arrive while the read is pending (paste/image/programmatic edit).
    if(this.dirty||epoch!==this.epoch){this.fail(Error('REVISION_CONFLICT'));return;}
    this.context=data.context;this.content=data.note.content;this.view.load(this.content);this.view.saved?.(data);
   }
   this.prepareId=null;this.unpause();
  }catch(error){if(this.isCurrentRead(generation,id))this.fail(error);}
 }
 refresh():Promise<void> {
  if(this.dirty||this.paused||this.conflicted)return Promise.resolve();
  this.paused=true;this.view.setEditable(false);
  return this.enqueueLifecycle(()=>this.refreshNow());
 }
 private async refreshNow() {
  if(this.dirty||this.conflicted||this.prepareId!==null)return;
  const epoch=this.epoch;const generation=++this.readGeneration;const prepareId=this.prepareId;
  this.paused=true;this.view.setEditable(false);
  try {
   const data=await this.transport.read(this.context);
   if(!this.isCurrentRead(generation,prepareId))return;
   if(this.dirty||epoch!==this.epoch){this.fail(Error('REVISION_CONFLICT'));return;}
   this.context=data.context;this.content=data.note.content;this.view.load(this.content);this.view.saved?.(data);
   this.unpause();
  }catch(error){if(this.isCurrentRead(generation,prepareId))this.fail(error);}
 }
 flush():Promise<void> {
  const work=this.chain.then(async()=>{
   if(this.failure)throw this.failure;
   if(!this.dirty)return;
   if(this.paused)throw Error('EDITOR_BUSY');
   const epoch=this.epoch;const content=this.content;
   const data=await this.transport.save(this.context,content);
   this.context=data.context;
   this.view.saved?.(data);
   if(this.epoch===epoch){this.dirty=false;this.view.persist(this.context,null);}
  }).catch(error=>{this.fail(error);throw error;});
  // Keep later barriers serialized; report rejection to the caller without an unhandled chain.
  this.chain=work.catch(()=>{});
  return work;
 }
 close():Promise<void>{
  if(this.closing)return this.closing;
  const closing=(async()=>{await this.flush();if(this.dirty)throw Error('EDITOR_BUSY');await this.transport.end(this.context);})();
  this.closing=closing;
  void closing.catch(()=>{if(this.closing===closing)this.closing=null;});
  return closing;
 }
}
