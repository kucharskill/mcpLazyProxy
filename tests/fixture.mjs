import { appendFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
const [log, mode] = process.argv.slice(2);
const record = (event, data={}) => appendFileSync(log, JSON.stringify({event,pid:process.pid,...data})+'\n');
record('start', {mode});
if(mode==='grandchild') { setInterval(()=>{},1000); }
else if(mode==='child') {
  spawn(process.execPath,[import.meta.filename,log,'grandchild'],{stdio:'ignore',windowsHide:true,detached:true});
  setInterval(()=>{},1000);
} else {
  const out = m => process.stdout.write(JSON.stringify({jsonrpc:'2.0',...m})+'\n');
  let initialized=false, calls=new Map(), rootsCall;
  const rl=createInterface({input:process.stdin});
  rl.on('line',line=>{
    const m=JSON.parse(line); record(m.method||'response', {id:m.id});
    if(m.method==='initialize') {
      out({id:m.id,result:{protocolVersion:m.params.protocolVersion, capabilities:{tools:{listChanged:true}},serverInfo:{name:'fixture',version:'1'},_meta:{ttlMs:0,opaque:[1,2]}}});
    } else if(m.method==='server/discover') {
      out({id:m.id,result:{supportedVersions:['2026-07-28'],capabilities:{tools:{listChanged:true}},extra:{preserve:true}}});
    } else if(m.method==='notifications/initialized') {
      initialized=true;
      if(mode==='pinging')setInterval(()=>out({id:'health',method:'ping'}),30);
    }
    else if(m.method==='tools/list') {
      out({id:m.id,result:{tools:[{name:'echo',inputSchema:{type:'object',properties:{session_id:{type:'string'}}}}],_meta:{ttlMs:0},extra:'original',...(m.params?.cursor?{page:m.params.cursor}:{nextCursor:'next'})}});
    } else if(m.method==='notifications/cancelled') {
      const timer=calls.get(m.params.requestId); clearTimeout(timer); calls.delete(m.params.requestId);
      out({id:m.params.requestId,error:{code:-32800,message:'cancelled'}});
    } else if(m.method==='tools/call') {
      const {name,arguments:a={}}=m.params;
      if(mode!=='modern' && !initialized) { out({id:m.id,error:{code:-32000,message:'missing initialized'}}); return; }
      if(name==='crash') { process.exit(9); }
      if(name==='tree') spawn(process.execPath,[import.meta.filename,log,'child'],{stdio:'ignore',windowsHide:true,detached:true});
      if(name==='roots') { rootsCall=m.id; out({id:7,method:'roots/list',params:{}}); return; }
      if(name==='background_roots')setTimeout(()=>out({id:'background-roots',method:'roots/list',params:{}}),400);
      if(name==='changed') out({method:'notifications/tools/list_changed'});
      const timer=setTimeout(()=>{ calls.delete(m.id); out({id:m.id,result:{content:[{type:'text',text:JSON.stringify({name,pid:process.pid,args:a})}],_meta:{opaque:'keep'}}}); }, a.delay||0);
      calls.set(m.id,timer);
    } else if(m.id==='background-roots' && !m.method) {
      // The background callback is complete; it has no outstanding tool request.
    } else if(m.id===7 && !m.method) {
      out({id:rootsCall,result:{content:[{type:'text',text:JSON.stringify(m.result)}]}});
    } else if(m.id!==undefined) out({id:m.id,error:{code:-32601,message:'unknown'}});
  });
  rl.on('close',()=>process.exit(0));
}
