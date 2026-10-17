import {readFileSync,readdirSync} from 'node:fs';
import {createHash} from 'node:crypto';
const origin='https://rd.killean.me';
const tag=process.env.REMOTE_DESK_RELEASE_TAG;
const version=JSON.parse(readFileSync('package.json','utf8')).version;
if(tag!==`v${version}`)throw Error('Release tag must match package version');
const records=readdirSync('artifacts').filter(f=>/^update-.*\.json$/.test(f)).map(f=>JSON.parse(readFileSync(`artifacts/${f}`,'utf8')));
const required=['darwin-aarch64','darwin-x86_64','windows-x86_64'];
if(records.length!==3||required.some(p=>records.filter(r=>r.platform===p).length!==1)||records.some(r=>r.version!==version||r.commit!==process.env.GITHUB_SHA))throw Error('All three signed packages must match this version and source commit');
async function identity(){
 if(!process.env.ACTIONS_ID_TOKEN_REQUEST_URL||!process.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN)throw Error('Publishing requires GitHub Actions OIDC permission');
 const url=new URL(process.env.ACTIONS_ID_TOKEN_REQUEST_URL);url.searchParams.set('audience',origin);
 const r=await fetch(url,{headers:{Authorization:`Bearer ${process.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN}`},signal:AbortSignal.timeout(30000)});
 if(!r.ok)throw Error('GitHub identity request failed');
 return (await r.json()).value;
}
async function post(path,body,type='application/octet-stream'){
 const token=await identity();
 const r=await fetch(origin+path,{method:'POST',headers:{Authorization:`Bearer ${token}`,'Content-Type':type},body,signal:AbortSignal.timeout(120000)});
 if(!r.ok)throw Error(`Update host rejected publishing: HTTP ${r.status}`);
 return r.json();
}
const platforms={};
for(const record of records){
 const bytes=readFileSync(`artifacts/${record.filename}`);
 platforms[record.platform]={filename:record.filename,signature:record.signature,sha256:createHash('sha256').update(bytes).digest('hex')};
}
// Every request uses a short-lived identity tied to this repository, branch and workflow.
for(const filename of readdirSync('artifacts').filter(f=>f.startsWith(`Remote-Desk-${version}-`)&&/\.(exe|dmg|gz|sha256|sig)$/.test(f))){
 if(!/^[A-Za-z0-9_.-]+$/.test(filename))throw Error('Invalid release filename');
 const bytes=readFileSync(`artifacts/${filename}`);
 for(let offset=0;offset<bytes.length;offset+=8*1024*1024){
   const end=Math.min(offset+8*1024*1024,bytes.length);
   const result=await post(`/api/upload/${version}/${filename}?offset=${offset}`,bytes.subarray(offset,end));
   if(result.received!==end)throw Error('Upload acknowledgement differs');
 }
 console.log(`Uploaded ${filename}`);
}
await post('/api/publish',JSON.stringify({version,commit:process.env.GITHUB_SHA,notes:'Remote Desk updates for Windows and macOS, with the bundled transfer engine.',platforms}),'application/json');
console.log(`Published verified release ${version} at ${origin}`);
