import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';
const atlas=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const root=path.resolve(atlas,'../..');
const context={window:{}};vm.createContext(context);
for(const file of ['atlas-data.js','snapshot.js'])vm.runInContext(fs.readFileSync(path.join(atlas,file),'utf8'),context,{filename:file});
const A=context.window.ATLAS,S=context.window.SNAPSHOT,errors=[];
const ids=new Set(A.nodes.map(n=>n.id));
if(ids.size!==A.nodes.length)errors.push('Yinelenen bileşen kimliği');
const checkSource=p=>{if(!S.sources[p])errors.push('Gömülmemiş kaynak: '+p);};
for(const n of A.nodes){if(!A.layers[n.layer])errors.push('Bilinmeyen katman: '+n.id);if(!n.sources.length)errors.push('Kanıtsız bileşen: '+n.id);n.sources.forEach(checkSource);}
for(const e of A.edges)if(!ids.has(e.from)||!ids.has(e.to))errors.push('Kırık ilişki: '+JSON.stringify(e));
for(const f of A.flows)for(const step of f.steps)if(!ids.has(step[0]))errors.push('Kırık akış: '+f.id);
for(const issue of A.issues)issue.slice(3).forEach(checkSource);
for(const rule of A.rules)checkSource(rule[2]);
for(const [p,s] of Object.entries(S.sources)){
 const file=path.join(root,p);
 if(!fs.existsSync(file)){errors.push('Silinmiş kaynak: '+p);continue;}
 const hash=crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex').slice(0,12);
 if(hash!==s.sha256)errors.push('Eski snapshot: '+p+' (refresh_snapshot.py çalıştırın)');
}
for(const service of S.services)checkSource(service.path);
for(const name of ['app.js','atlas-data.js','snapshot.js'])new vm.Script(fs.readFileSync(path.join(atlas,name),'utf8'),{filename:name});
const html=fs.readFileSync(path.join(atlas,'index.html'),'utf8');
for(const match of html.matchAll(/(?:src|href)="([^"#]+)"/g)){if(!fs.existsSync(path.join(atlas,match[1])))errors.push('Kırık statik varlık: '+match[1]);}
console.log(JSON.stringify({ok:errors.length===0,nodes:A.nodes.length,edges:A.edges.length,flows:A.flows.length,sources:Object.keys(S.sources).length,documents:S.documents.length,services:S.services.length,inventory:S.inventory.length,errors},null,2));
if(errors.length)process.exitCode=1;
