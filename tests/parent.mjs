// Leave another process holding the proxy's stdin open after this parent is killed.
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
const [binary,fixture,dir]=process.argv.slice(2);
const p=spawn(binary,['--cache-dir',dir+'/cache','--shutdown-ms','100','--',process.execPath,fixture,dir+'/events.jsonl','server'],{windowsHide:true});
const holder=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:['ignore',p.stdin,'inherit'],windowsHide:true,detached:true});
p.stderr.resume();
const send=m=>p.stdin.write(JSON.stringify({jsonrpc:'2.0',...m})+'\n');
createInterface({input:p.stdout}).on('line',line=>{
  const m=JSON.parse(line);
  if(m.id===1){send({method:'notifications/initialized'});send({id:2,method:'tools/call',params:{name:'tree'}});}
  if(m.id===2)console.log(JSON.stringify({proxy:p.pid,holder:holder.pid}));
});
send({id:1,method:'initialize',params:{protocolVersion:'2025-11-25',capabilities:{},clientInfo:{name:'parent-test',version:'1'}}});
