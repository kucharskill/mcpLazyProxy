import {test} from 'node:test';
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdir,readFile,mkdtemp,copyFile,rename,writeFile,access,readdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {setTimeout as delay} from 'node:timers/promises';

const binary=resolve(process.env.LAZY_BINARY||'target/debug/mcp-lazy-proxy.exe');
const fixture=resolve('tests/fixture.mjs');
await mkdir('.tmp/tests',{recursive:true});
const initParams={protocolVersion:'2025-11-25',capabilities:{roots:{listChanged:true}},clientInfo:{name:'tests',version:'1'}};
async function workspace(){return mkdtemp(resolve('.tmp/tests/run-'));}
async function records(dir){try{return (await readFile(join(dir,'events.jsonl'),'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse);}catch{return [];}}
async function until(fn,message='condition',ms=5000){const end=Date.now()+ms;while(Date.now()<end){if(await fn())return;await delay(25);}throw Error('timeout: '+message);}
function alive(pid){try{process.kill(pid,0);return true;}catch{return false;}}
function connect(dir,{env={},extra=[],mode='server',executable=binary}={}){
  const child=spawn(executable,['--cache-dir',join(dir,'cache'),'--idle-ms','500','--discovery-idle-ms','120','--shutdown-ms','100',...extra,'--',process.execPath,fixture,join(dir,'events.jsonl'),mode],{env:{...process.env,...env},windowsHide:true});
  let sequence=0,stderr='',messages=[],pending=new Map();
  child.stderr.on('data',data=>stderr+=data); child.on('error',()=>{});
  const send=m=>child.stdin.write(JSON.stringify({jsonrpc:'2.0',...m})+'\n');
  createInterface({input:child.stdout}).on('line',line=>{
    const m=JSON.parse(line); messages.push(m);
    if(m.method==='roots/list')send({id:m.id,result:{roots:[{uri:'file:///test'}]}});
    else if(pending.has(m.id)){pending.get(m.id)(m);pending.delete(m.id);}
  });
  function request(method,params={},id=++sequence){
    return new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>{pending.delete(id);reject(Error(`timeout ${method}: ${stderr}`));},5000);
      pending.set(id,m=>{clearTimeout(timer);resolve(m);}); send({id,method,params});
    });
  }
  return {child,request,send,messages,get stderr(){return stderr;},
    async init(){const m=await request('initialize',initParams);assert.ok(m.result,JSON.stringify(m));send({method:'notifications/initialized'});return m.result;},
    async close(){child.stdin.end();await until(()=>child.exitCode!==null||child.signalCode!==null,'proxy exit');assert.equal(child.exitCode,0,stderr);},
  };
}
const call=(p,name,args={})=>p.request('tools/call',{name,arguments:args});
test('version is readable through a captured native process',()=>{
  const result=spawnSync(binary,['--version'],{encoding:'utf8',windowsHide:true});
  assert.equal(result.status,0,result.stderr);assert.equal(result.stderr,'');
  if(process.env.MCP_LAZY_RELEASE_VERSION)assert.equal(result.stdout.trim(),'mcp-lazy-proxy '+process.env.MCP_LAZY_RELEASE_VERSION);
  else assert.match(result.stdout.trim(),/^mcp-lazy-proxy \d+\.\d+\.\d+(?:-build\.\d+)?$/);
});
test('running image can be renamed and replaced; startup cleans only unused marked backups',{skip:process.platform!=='win32'},async()=>{
  const dir=await workspace(),installed=join(dir,'mcp-lazy-proxy.exe');
  const retired=join(dir,'mcp-lazy-proxy.old-0123456789abcdef0123456789abcdef.exe');
  const unrelated=join(dir,'mcp-lazy-proxy.old-user-file.exe');
  await copyFile(binary,installed);await writeFile(unrelated,'keep');
  const p=connect(dir,{executable:installed});let next;
  try{
    await p.request('ping');await rename(installed,retired);await copyFile(binary,installed);
    next=connect(dir,{executable:installed});await next.request('ping');await access(retired);assert.equal(next.stderr,'','locked-backup cleanup must be silent');
    await p.request('ping'); // Renaming/replacing the image has not interrupted the old session.
    await p.close();await next.close();next=null;
    next=connect(dir,{executable:installed});await next.request('ping');
    await assert.rejects(access(retired));await access(unrelated);
  }finally{
    if(p.child.exitCode===null&&p.child.signalCode===null)await p.close();
    if(next)await next.close();
  }
});
function text(m){assert.ok(m.result,JSON.stringify(m));return JSON.parse(m.result.content[0].text);}

test('ping never starts backend',async()=>{
  const dir=await workspace(),p=connect(dir);try{assert.deepEqual((await p.request('ping')).result,{});assert.equal((await records(dir)).length,0);}finally{await p.close();}
});
test('backend pings are answered locally without preventing idle shutdown',async()=>{
  const dir=await workspace(),p=connect(dir,{mode:'pinging'});
  try{await p.init();const pid=text(await call(p,'echo',{delay:120})).pid;await until(()=>!alive(pid));assert.ok(!p.messages.some(m=>m.method==='ping'));assert.ok((await records(dir)).some(m=>m.event==='response'&&m.id==='health'));}
  finally{await p.close();}
});
test('stateful backend can disable idle shutdown explicitly',async()=>{
  const dir=await workspace(),p=connect(dir,{extra:['--idle-ms','0']});
  try{await p.init();const first=text(await call(p,'echo'));await delay(700);assert.ok(alive(first.pid));assert.equal(text(await call(p,'echo')).pid,first.pid);}
  finally{await p.close();}
});
test('discovery-only backends stop immediately regardless of cache TTL and idle=0',async()=>{
  for(const ttl of ['0','600000']){
    const dir=await workspace(),p=connect(dir,{extra:['--idle-ms','0','--discovery-idle-ms','60000','--cache-ttl-ms',ttl]});
    const stopped=async()=>{
      const pids=(await records(dir)).filter(x=>x.event==='start').map(x=>x.pid);
      assert.ok(pids.length);await until(()=>pids.every(pid=>!alive(pid)),'discovery backend stops immediately',1500);
    };
    try{
      await p.init();await stopped();await p.request('tools/list');await stopped();
      const starts=(await records(dir)).filter(x=>x.event==='start').length;
      await p.request('tools/list');await stopped();
      assert.equal((await records(dir)).filter(x=>x.event==='start').length,starts+(ttl==='0'?1:0));
      const operational=text(await call(p,'echo'));
      await p.request('tools/list');await p.request('ping');await delay(250);
      assert.ok(alive(operational.pid),'discovery must not terminate an operational backend');
      assert.equal(text(await call(p,'echo')).pid,operational.pid);
    }finally{await p.close();}
  }
});
test('overlapping discovery and a real operation keep the backend alive',async()=>{
  const dir=await workspace(),p=connect(dir,{extra:['--idle-ms','0']});
  try{
    await p.init();
    const listing=p.request('tools/list',{});
    const operation=call(p,'echo',{delay:200});
    assert.ok((await listing).result.tools.length);
    const result=text(await operation);await delay(250);assert.ok(alive(result.pid));
    assert.equal(text(await call(p,'echo')).pid,result.pid);
  }finally{await p.close();}
});
test('discovery and pings cannot renew the idle deadline after a real response',async()=>{
  const dir=await workspace(),p=connect(dir,{extra:['--idle-ms','600','--discovery-idle-ms','5000']});
  try{
    await p.init();const result=text(await call(p,'echo'));await delay(350);
    await p.request('tools/list');await p.request('ping');
    assert.ok(alive(result.pid),'discovery must not immediately terminate an operational backend');
    await until(()=>!alive(result.pid),'original real-operation idle deadline',600);
  }finally{await p.close();}
});
test('background server callbacks protect pending work but do not renew client-operation idle',async()=>{
  const dir=await workspace(),p=connect(dir,{extra:['--idle-ms','800']});
  try{
    await p.init();const result=text(await call(p,'background_roots'));await delay(500);
    assert.ok((await records(dir)).some(m=>m.event==='response'&&m.id==='background-roots'));
    await until(()=>!alive(result.pid),'idle counted from the last real client response',650);
  }finally{await p.close();}
});
test('response timeout bounds a stuck request independently of idle and other replies',async()=>{
  const dir=await workspace(),p=connect(dir,{extra:['--idle-ms','80','--response-timeout-ms','600']});
  try{
    await p.init();const started=Date.now();
    const stuck=p.request('tools/call',{name:'echo',arguments:{delay:3000}},'stuck');
    const fast=text(await call(p,'echo'));await delay(250);
    assert.ok(alive(fast.pid),'active request protects against idle');
    text(await call(p,'echo')); // Completing another operation must not renew the stuck request's limit.
    const response=await stuck;
    assert.equal(response.id,'stuck');assert.equal(response.error.code,-32000);assert.match(response.error.message,/response timeout/);
    assert.ok(Date.now()-started>=550);assert.ok(Date.now()-started<2000);
    await until(()=>!alive(fast.pid),'timed-out backend exits');
    assert.notEqual(text(await call(p,'echo')).pid,fast.pid);
  }finally{await p.close();}
});
test('response timeout still works when automatic idle shutdown is disabled',async()=>{
  const dir=await workspace(),p=connect(dir,{extra:['--idle-ms','0','--response-timeout-ms','400']});
  try{
    await p.init();const response=await call(p,'echo',{delay:2000});
    assert.match(response.error.message,/response timeout/);text(await call(p,'echo'));
  }finally{await p.close();}
});
test('warm cache across replicas preserves all result fields and ignores agreed env noise',async()=>{
  const dir=await workspace();let p=connect(dir),init,list;
  try{init=await p.init();list=(await p.request('tools/list')).result;}finally{await p.close();}
  p=connect(dir,{env:{PATH:process.env.PATH+';unused',INIT_CWD:'different',npm_config_test:'different',CODEX_TOOL_CALL_ID:'different'}});
  try{assert.deepEqual(await p.init(),init);assert.deepEqual((await p.request('tools/list',{},'new-id')).result,list);assert.equal((await records(dir)).filter(x=>x.event==='start').length,2);assert.match(p.stderr,/cache hit/);}
  finally{await p.close();}
});
test('idle tree cleanup, handshake replay, concurrent replies, ping and discovery cannot shorten tool grace',async()=>{
  const dir=await workspace(),p=connect(dir);
  try{
    await p.init();await p.request('tools/list');
    await until(()=>p.stderr.includes('backend stopped'),'discovery idle');
    const first=text(await call(p,'tree'));
    await until(async()=> (await records(dir)).some(x=>x.mode==='grandchild'),'grandchild start');
    const slow=call(p,'echo',{delay:700});await delay(200);await p.request('tools/list');await p.request('ping');
    const fast=await call(p,'echo',{delay:10});assert.equal(text(fast).pid,first.pid);
    assert.equal(text(await slow).pid,first.pid);await delay(230);assert.ok(alive(first.pid));
    await until(()=>!alive(first.pid),'tool idle');
    const pids=(await records(dir)).filter(x=>x.event==='start').map(x=>x.pid);
    await until(()=>pids.every(pid=>!alive(pid)),'all idle descendants exit');
    const next=text(await call(p,'echo'));assert.notEqual(next.pid,first.pid);
    assert.equal((await records(dir)).filter(x=>x.event==='initialize').length,4);
  }finally{await p.close();}
});
test('server requests and cancellation IDs survive proxy routing',async()=>{
  const dir=await workspace(),p=connect(dir);
  try{
    await p.init();assert.deepEqual(text(await call(p,'roots')).roots,[{uri:'file:///test'}]);
    const cancelled=p.request('tools/call',{name:'echo',arguments:{delay:2000}},'cancel-me');
    await delay(80);p.send({method:'notifications/cancelled',params:{requestId:'cancel-me',reason:'test'}});
    assert.equal((await cancelled).error.code,-32800);
  }finally{await p.close();}
});
test('cache partitions pages and invalidates dynamic tools list',async()=>{
  const dir=await workspace();let p=connect(dir);
  try{await p.init();await p.request('tools/list');assert.equal((await p.request('tools/list',{cursor:'next'})).result.page,'next');text(await call(p,'changed'));}finally{await p.close();}
  p=connect(dir);try{await p.init();await p.request('tools/list');assert.equal((await records(dir)).filter(x=>x.event==='tools/list').length,3);}finally{await p.close();}
});
test('expired cache starts backend; changed backend env gets separate cache',async()=>{
  const dir=await workspace();let p=connect(dir,{extra:['--cache-ttl-ms','100']});try{await p.init();}finally{await p.close();}
  await delay(150);p=connect(dir,{extra:['--cache-ttl-ms','100']});try{await p.init();}finally{await p.close();}
  p=connect(dir,{env:{LAZY_TEST_CONFIG:'different'}});try{await p.init();}finally{await p.close();}
  assert.equal((await records(dir)).filter(x=>x.event==='start').length,3);
});
test('backend crash fails outstanding operation without retry, next operation can reconnect',async()=>{
  const dir=await workspace(),p=connect(dir);try{
    await p.init();const m=await call(p,'crash');assert.equal(m.error.code,-32000);
    assert.equal((await records(dir)).filter(x=>x.event==='tools/call').length,1);
    text(await call(p,'echo'));
  }finally{await p.close();}
});
test('hard killing proxy leaves no descendant processes on Windows',{skip:process.platform!=='win32'},async()=>{
  const dir=await workspace(),p=connect(dir);let pids=[];
  try{
    await p.init();text(await call(p,'tree'));await until(async()=> (await records(dir)).some(x=>x.mode==='grandchild'));
    pids=(await records(dir)).filter(x=>x.event==='start').map(x=>x.pid);p.child.kill('SIGKILL');
    await until(()=>pids.every(pid=>!alive(pid)),'job cleanup');
  }finally{if(p.child.exitCode===null&&p.child.signalCode===null)p.child.kill('SIGKILL');for(const pid of pids)if(alive(pid))process.kill(pid,'SIGKILL');}
});
test('modern discovery and tools cache preserve original discovery',{timeout:10000},async()=>{
  const dir=await workspace();let p=connect(dir,{mode:'modern'}),discovery;
  const params={_meta:{'io.modelcontextprotocol/protocolVersion':'2026-07-28'}};
  try{discovery=(await p.request('server/discover',params)).result;await p.request('tools/list',params);}finally{await p.close();}
  p=connect(dir,{mode:'modern'});try{assert.deepEqual((await p.request('server/discover',params)).result,discovery);await p.request('tools/list',params);assert.equal((await records(dir)).filter(x=>x.event==='start').length,2);text(await call(p,'echo'));}finally{await p.close();}
});
test('parent death shuts down proxy even when another process keeps stdin open',{skip:process.platform!=='win32'},async()=>{
  const dir=await workspace();let info,pids=[];
  const parent=spawn(process.execPath,[resolve('tests/parent.mjs'),binary,fixture,dir],{windowsHide:true});parent.stderr.resume();
  try{
    createInterface({input:parent.stdout}).on('line',line=>{info=JSON.parse(line);});
    await until(()=>info,'parent startup');await until(async()=> (await records(dir)).some(x=>x.mode==='grandchild'));
    pids=(await records(dir)).filter(x=>x.event==='start').map(x=>x.pid);
    assert.ok(alive(info.holder),'holder starts alive');
    parent.kill('SIGKILL');
    await until(()=>!alive(info.proxy)&&pids.every(pid=>!alive(pid)),'parent death cleanup');
    assert.ok(alive(info.holder),'holder kept stdin open');
  }finally{
    parent.kill('SIGKILL');for(const pid of [...pids,info?.proxy,info?.holder].filter(Boolean))if(alive(pid))process.kill(pid,'SIGKILL');
  }
});


test('diagnostic control targets one process and defers exit during a request',async()=>{
  const dir=await workspace(),root=join(dir,'cache','diagnostics');
  const p=connect(dir,{extra:['--idle-ms','0']}),other=connect(await workspace());
  try{
    await p.init();await other.request('ping');
    const entries=await readdir(root);assert.equal(entries.length,1);
    const inbox=join(root,entries[0]);
    const status=()=>readFile(join(inbox,'status.json'),'utf8').then(JSON.parse);
    assert.equal((await status()).pid,p.child.pid);
    const oldPid=text(await call(p,'echo')).pid;
    await writeFile(join(inbox,'command'),'stop-backend');
    await until(async()=> (await status()).state==='accepted');
    await until(()=>!alive(oldPid),'controlled backend exit');
    assert.notEqual(text(await call(p,'echo')).pid,oldPid);
    const response=call(p,'echo',{delay:1800});await delay(100);
    await writeFile(join(inbox,'command'),'exit');
    await until(async()=> (await status()).state==='pending');
    assert.equal(p.child.exitCode,null);assert.ok((await response).result);
    await until(()=>p.child.exitCode!==null,'diagnostic exit');assert.equal(p.child.exitCode,0);
    assert.deepEqual((await other.request('ping')).result,{});
    await until(async()=> (await readdir(root)).length===0,'diagnostic files cleaned');
  }finally{if(p.child.exitCode===null)await p.close();await other.close();}
});

test('diagnostic fail closes transport with a nonzero exit code',async()=>{
  const dir=await workspace(),root=join(dir,'cache','diagnostics'),p=connect(dir);
  try{
    await p.request('ping');const [entry]=await readdir(root);
    await writeFile(join(root,entry,'command'),'fail');
    await until(()=>p.child.exitCode!==null,'diagnostic failure');assert.equal(p.child.exitCode,1);
  }finally{if(p.child.exitCode===null)await p.close();}
});
