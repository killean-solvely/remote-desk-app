import React, {useEffect, useRef, useState} from 'react';
import {createRoot} from 'react-dom/client';
import {invoke} from '@tauri-apps/api/core';
import {listen} from '@tauri-apps/api/event';
import './style.css';
import {Updates} from './Updates';
import {formatEta, formatPath} from './format';

type Remote={id:string;label:string;host:string;port:number;user:string;root:string;auth:string;knownHosts:string};
type HostKey={token:string;host:string;port:number;algorithm:string;fingerprint:string};
type Entry={Path:string;Name:string;Size:number;IsDir:boolean;ModTime:string};
type FolderSize={bytes:number;count:number};
type SizeScan={id:string;key:string;remoteId:string;path:string;canceled:boolean};
const sizeKey=(id:string,p:string)=>JSON.stringify([id,p]);
// WKWebView custom origins need not expose secure-context-only randomUUID.
const sizeRequestId=()=>{
 const bytes=crypto.getRandomValues(new Uint8Array(16));bytes[6]=(bytes[6]&15)|64;bytes[8]=(bytes[8]&63)|128;
 const hex=Array.from(bytes,b=>b.toString(16).padStart(2,'0')).join('');
 return [hex.slice(0,8),hex.slice(8,12),hex.slice(12,16),hex.slice(16,20),hex.slice(20)].join('-');
};
const childPath=(parent:string,name:string)=>[parent,name].filter(Boolean).join('/');
type Stats={bytes?:number;totalBytes?:number;speed?:number;eta?:number;transferring?:{name:string;bytes:number;size:number;speed:number;percentage:number}[]};
type Job={id:string;remoteId:string;path:string;destination:string;state:string;detail:string;stats:Stats};
type Settings={rclone:string;transfers:number;streams:number;cutoff:string;chunk:string;bandwidth:string};
const blankRemote:Remote={id:'',label:'',host:'',port:22,user:'',root:'',auth:'agent',knownHosts:''};
const formatBytes=(n=0)=>{const i=Math.max(0,Math.min(4,Math.floor(Math.log2(n||1)/10)));return `${(n/1024**i).toFixed(i?1:0)} ${['B','KiB','MiB','GiB','TiB'][i]}`};
const active=(j:Job)=>['queued','preparing','running'].includes(j.state);

function App(){
 const [tab,setTab]=useState('browse'),[remotes,setRemotes]=useState<Remote[]>([]),[remoteId,setRemoteId]=useState(''),[path,setPath]=useState(''),[entries,setEntries]=useState<Entry[]>([]),[selected,setSelected]=useState<string[]>([]),[jobs,setJobs]=useState<Job[]>([]),[settings,setSettings]=useState<Settings>({rclone:'',transfers:4,streams:16,cutoff:'100M',chunk:'16M',bandwidth:''}),[draft,setDraft]=useState<Remote>(blankRemote),[verified,setVerified]=useState(false),[busy,setBusy]=useState(false),[message,setMessage]=useState(''),[error,setError]=useState(''),[filter,setFilter]=useState(''),[destination,setDestination]=useState(''),[sort,setSort]=useState('name');
 const [editing,setEditing]=useState(false),[deleting,setDeleting]=useState(''),[hostKey,setHostKey]=useState<HostKey|null>(null);
 const [updateVersion,setUpdateVersion]=useState('');
 const [folderSizes,setFolderSizes]=useState<Record<string,FolderSize>>({}),[sizeScan,setSizeScan]=useState<SizeScan|null>(null);
 const sizeScanRef=useRef<SizeScan|null>(null);
 function clearFolderSizes(id:string){setFolderSizes(cache=>Object.fromEntries(Object.entries(cache).filter(([key])=>JSON.parse(key)[0]!==id)))}
 async function calculateSize(entry:Entry){
  if(sizeScanRef.current)return;
  const p=childPath(path,entry.Name),key=sizeKey(remoteId,p);
  const scan:SizeScan={id:sizeRequestId(),key,remoteId,path:p,canceled:false};
  sizeScanRef.current=scan;setSizeScan(scan);setError('');
  setFolderSizes(cache=>{const next={...cache};delete next[key];return next});
  try{
   const result=await invoke<FolderSize>('calculate_folder_size',{remoteId:scan.remoteId,path:p,requestId:scan.id});
   if(!scan.canceled)setFolderSizes(cache=>({...cache,[key]:result}));
  }catch(e){if(!scan.canceled)fail(`Could not calculate size for ${p}: ${String(e)}`)}
  finally{if(sizeScanRef.current===scan){sizeScanRef.current=null;setSizeScan(null)}}
 }
 async function cancelSize(){
  const scan=sizeScanRef.current;if(!scan||scan.canceled)return;
  scan.canceled=true;setSizeScan({...scan});
  try{await invoke('cancel_folder_size',{requestId:scan.id})}catch(e){fail(e)}
 }
 function entrySize(entry:Entry){return entry.IsDir?folderSizes[sizeKey(remoteId,childPath(path,entry.Name))]?.bytes??-1:entry.Size}

 const [fileManager,setFileManager]=useState('file manager');
 const [locationPending,setLocationPending]=useState<Record<string,'reveal'|'folder'>>({}),[locationErrors,setLocationErrors]=useState<Record<string,string>>({});
 const locationRequests=useRef(new Set<string>());
 async function openLocation(job:Job,kind:'reveal'|'folder'){
  if(locationRequests.current.has(job.id))return;
  locationRequests.current.add(job.id);setLocationPending(p=>({...p,[job.id]:kind}));
  setLocationErrors(p=>{const next={...p};delete next[job.id];return next});
  try{await invoke('open_download_location',{id:job.id,action:kind})}
  catch(e){setLocationErrors(p=>({...p,[job.id]:String(e)}))}
  finally{locationRequests.current.delete(job.id);setLocationPending(p=>{const next={...p};delete next[job.id];return next})}
 }
 const generation=useRef(0);const remote=remotes.find(r=>r.id===remoteId);
 const fail=(e:unknown)=>setError(String(e));
 async function refreshJobs(){setJobs(await invoke<Job[]>('list_jobs'))}
 useEffect(()=>{void invoke<string>('file_manager_name').then(setFileManager).catch(()=>{});Promise.all([invoke<Remote[]>('list_remotes'),invoke<Settings>('get_settings'),invoke<Job[]>('list_jobs')]).then(([r,s,j])=>{setRemotes(r);setSettings(s);setJobs(j);}).catch(fail);
  let disposed=false;const off=Promise.all(['job_started','job_progress','job_completed','job_failed','job_canceled'].map(name=>listen<Job>(name,e=>{if(!disposed){if(name==='job_failed')setError(`Download failed: ${e.payload.detail} See History for details.`);setJobs(js=>{const index=js.findIndex(j=>j.id===e.payload.id);return index<0?[...js,e.payload]:js.map(j=>j.id===e.payload.id?e.payload:j)})}})));return()=>{disposed=true;void off.then(fs=>fs.forEach(f=>f()))};},[]);
 async function browse(id:string,p:string){const token=++generation.current;setBusy(true);setError('');setSelected([]);setEntries([]);setPath(p);try{const result=await invoke<Entry[]>('list_directory',{remoteId:id,path:p});if(token===generation.current)setEntries(result)}catch(e){if(token===generation.current)fail(e)}finally{if(token===generation.current)setBusy(false)}}
 async function action(fn:()=>Promise<void>){setError('');setBusy(true);try{await fn()}catch(e){fail(e)}finally{setBusy(false)}}
 async function pick(set:(s:string)=>void,command='select_file'){const p=await invoke<string|null>(command);if(p)set(p)}
 async function queue(){if(!remoteId||selected.length===0)return;let target=destination;if(!target){target=await invoke<string|null>('select_destination')||'';if(!target)return;setDestination(target)}
  // This click explicitly authorizes downloading the displayed selection.
  for(const item of selected){const p=[path,item].filter(Boolean).join('/');await invoke('enqueue_download',{remoteId,path:p,destination:target})}await refreshJobs();setMessage(`Queued ${selected.length} item(s). Each receives an isolated destination folder.`);setSelected([])
 }
 async function password(id:string){const saved=await invoke<boolean>('set_remote_password',{remoteId:id});setMessage(saved?'Password saved securely. Test the connection when ready.':'Password entry canceled. No new password was saved.')}
 function resetForm(){setDraft({...blankRemote});setEditing(false);setVerified(false);setHostKey(null)}
 async function fetchKey(){setHostKey(null);setVerified(false);setDraft(d=>({...d,knownHosts:''}));const key=await invoke<HostKey>('fetch_host_key',{host:draft.host,port:draft.port});setHostKey(key)}
 async function saveRemote(){
  const previous=remotes.find(r=>r.id===draft.id);
  const r=await invoke<Remote>(editing?'update_remote':'add_remote',{remote:draft,verified,replaceHosts:!!draft.knownHosts||!!hostKey,hostKeyToken:hostKey?.token||null});
  clearFolderSizes(r.id);
  setRemotes(rs=>editing?rs.map(old=>old.id===r.id?r:old):[...rs,r]);
  if(remoteId===r.id){generation.current++;setEntries([]);setPath('');setSelected([])}
  const needsPassword=r.auth==='password'&&(!editing||previous?.auth!=='password'||previous?.host!==r.host||previous?.port!==r.port||previous?.user!==r.user);
  resetForm();setMessage('Connection saved. Saving does not connect.');
  if(needsPassword)await password(r.id);
 }
 async function removeRemote(id:string){await invoke('delete_remote',{remoteId:id});clearFolderSizes(id);setRemotes(rs=>rs.filter(r=>r.id!==id));setDeleting('');if(remoteId===id){generation.current++;setRemoteId('');setEntries([]);setPath('');setSelected([])}if(draft.id===id)resetForm();setMessage('Connection and saved password deleted. Downloads and history were kept.')}
 function folderSizeCell(entry:Entry){
  const key=sizeKey(remoteId,childPath(path,entry.Name)),cached=folderSizes[key],scanning=sizeScan?.key===key;
  return <div className="folder-size">{cached&&<span title={`${cached.count} files - Cached for this session`}>{formatBytes(cached.bytes)}</span>}{scanning?<span role="status">{sizeScan?.canceled?'Canceling...':'Calculating...'}</span>:<button className="size-action" disabled={!!sizeScan||busy} aria-label={`${cached?'Recalculate':'Calculate'} size of ${entry.Name}`} onClick={()=>void calculateSize(entry)}>{cached?'Recalculate':'Calculate size'}</button>}</div>
 }
 const visible=entries.filter(e=>e.Name.toLowerCase().includes(filter.toLowerCase())).sort((a,b)=>Number(b.IsDir)-Number(a.IsDir)||(sort==='size'?entrySize(b)-entrySize(a):sort==='modified'?b.ModTime.localeCompare(a.ModTime):a.Name.localeCompare(b.Name)));
 const displayedJobs=tab==='history'?jobs.filter(j=>!active(j)):jobs.filter(active);
 return <div className="shell"><aside><div className="brand"><span className="logo">↙</span> Remote Desk<small>DIRECT SFTP · LOCAL DOWNLOADS</small></div><nav>{[['browse','▦','Remote files'],['downloads','↓','Downloads'],['history','◷','History'],['settings','⚙','Settings']].map(([key,icon,label])=><button className={tab===key?'current':''} onClick={()=>setTab(key)} key={key}><span>{icon}</span>{label}{key==='downloads'&&<b>{jobs.filter(active).length}</b>}</button>)}</nav><div className="side-label">YOUR REMOTES <button title="Add a remote" onClick={()=>setTab('settings')}>+</button></div>{remotes.map(r=><button className={`remote ${remoteId===r.id?'chosen':''}`} key={r.id} onClick={()=>{setRemoteId(r.id);setTab('browse');setEntries([]);setPath('');setSelected([]);generation.current++;setBusy(false)}}><span>▱</span><div>{r.label||r.host}<small>{r.host}</small></div></button>)}<div className="side-footer"><span className="dot"/> Runs on this computer<small>File bytes travel directly over SSH.</small></div></aside>
 <main><header><div><div className="eyebrow">WORKSPACE / {tab.toUpperCase()}</div><h1>{{browse:'Remote files',downloads:'Downloads',history:'Transfer history',settings:'Connection & transfer settings'}[tab]}</h1></div><span className="pill">SFTP / SSH</span></header>
 {updateVersion&&tab!=='settings'&&<div className="alert" role="status">Remote Desk {updateVersion} is available. <button onClick={()=>setTab('settings')}>View update</button></div>}
 {error&&<div role="alert" className="alert error">{error}<button aria-label="Dismiss error" onClick={()=>setError('')}>×</button></div>}{message&&<div role="status" className="alert">{message}<button aria-label="Dismiss notification" onClick={()=>setMessage('')}>×</button></div>}
 {tab==='browse'&&<><section className="connection"><div><strong>{remote?.label||'Choose a remote'}</strong><p>{remote?`${remote.user}@${remote.host}:${remote.port} · ${remote.root||'home directory'}`:'Add a verified SFTP connection in Settings to get started.'}</p></div><button disabled={!remoteId||busy} onClick={()=>void browse(remoteId,path)}>{busy?'Loading…':'Connect / refresh'}</button></section><div className="toolbar"><div className="breadcrumbs"><button disabled={busy||!remoteId} onClick={()=>void browse(remoteId,'')}>⌂ Root</button>{path.split('/').filter(Boolean).map((p,i,a)=><React.Fragment key={i}><span>/</span><button disabled={busy} onClick={()=>void browse(remoteId,a.slice(0,i+1).join('/'))}>{p}</button></React.Fragment>)}</div><input aria-label="Filter files" placeholder="Filter this folder…" value={filter} onChange={e=>setFilter(e.target.value)}/></div>
 <div className="selection"><span>{selected.length?`${selected.length} selected`:`${entries.length} items`} <small>· Symlinks excluded</small></span><div><button onClick={()=>void action(()=>pick(setDestination,'select_destination'))}>{destination?'Change destination':'Choose destination'}</button><button className="primary" disabled={busy||!selected.length} onClick={()=>void action(queue)}>↓ Download selected</button></div></div>
 {sizeScan&&<div className="size-status" role="status"><span>{sizeScan.canceled?'Canceling�':'Calculating�'} {remotes.find(r=>r.id===sizeScan.remoteId)?.label}: {sizeScan.path}</span><button disabled={sizeScan.canceled} onClick={()=>void cancelSize()}>Cancel size scan</button></div>}
 <p className="size-help muted">Folder sizes are calculated on demand from metadata. Symlinks are excluded. Results are cached for this session; recalculate to check for changes.</p>
 <div className="file-table"><table><thead><tr><th><input type="checkbox" aria-label="Select all visible files" checked={visible.length>0&&visible.every(e=>selected.includes(e.Name))} onChange={e=>setSelected(e.target.checked?visible.map(e=>e.Name):[])}/></th><th><button onClick={()=>setSort('name')}>Name ↕</button></th><th>Type</th><th><button onClick={()=>setSort('size')}>Size ↕</button></th><th><button onClick={()=>setSort('modified')}>Modified ↕</button></th></tr></thead><tbody>{visible.map(e=><tr key={e.Path}><td><input type="checkbox" aria-label={`Select ${e.Name}`} checked={selected.includes(e.Name)} onChange={ev=>setSelected(items=>ev.target.checked?[...items,e.Name]:items.filter(x=>x!==e.Name))}/></td><td><button className="filename" disabled={!e.IsDir||busy} onClick={()=>void browse(remoteId,[path,e.Name].filter(Boolean).join('/'))}><span>{e.IsDir?'▰':'▤'}</span>{e.Name}</button></td><td>{e.IsDir?'Folder':'File'}</td><td>{e.IsDir?folderSizeCell(e):formatBytes(e.Size)}</td><td>{e.ModTime?new Date(e.ModTime).toLocaleString():'—'}</td></tr>)}</tbody></table>{!visible.length&&<div className="empty"><span>▱</span><h2>{busy?'Reading remote directory…':remote?'Ready to browse':'Your files, one secure connection away'}</h2><p>{remote?'Click Connect / refresh to read this directory. Empty directories also appear here.':'Add a remote, verify its host key, then connect when you are ready.'}</p></div>}</div>{destination&&<p className="muted">Destination: {formatPath(destination)}</p>}</>}
 {(tab==='downloads'||tab==='history')&&<>{tab==='history'&&<div className="row"><p className="muted">Removing history never deletes downloaded files.</p><button disabled={busy||!jobs.some(j=>j.state==='completed')} onClick={()=>void action(async()=>{const count=await invoke<number>('clear_completed');await refreshJobs();setMessage(`Removed ${count} completed history item(s). Downloaded files were kept.`)})}>Clear completed</button></div>}<p className="muted">One job at a time. Each job uses {settings.transfers} concurrent files and up to {settings.streams} streams per eligible file.</p>{!displayedJobs.length&&<div className="empty"><span>↓</span><h2>{tab==='history'?'No finished transfers yet':'Your queue is clear'}</h2><p>Select remote files or folders to start a download.</p></div>}{displayedJobs.slice().reverse().map(j=><section className="job" key={j.id}><div className="job-title"><strong>{j.path}</strong><span className="pill">{j.state}</span></div><p>{formatPath(j.destination)}</p><progress max={Math.max(j.stats.totalBytes||0,1)} value={j.state==='completed'?Math.max(j.stats.totalBytes||0,1):j.stats.bytes||0}/><div className="job-metrics"><span>{formatBytes(j.stats.bytes)} / {formatBytes(j.stats.totalBytes)}</span><span>{formatBytes(j.stats.speed)}/s</span><span>ETA {formatEta(j.stats.eta)}</span>{active(j)?<button onClick={()=>void action(async()=>{await invoke('cancel_job',{id:j.id});await refreshJobs()})}>Cancel</button>:['failed','interrupted','canceled'].includes(j.state)&&remotes.some(r=>r.id===j.remoteId)&&<button onClick={()=>void action(async()=>{await invoke('retry_job',{id:j.id});await refreshJobs()})}>Retry</button>}{!active(j)&&<button disabled={busy} onClick={()=>void action(async()=>{await invoke('delete_history',{id:j.id});await refreshJobs();setMessage('History entry removed. Downloaded files were kept.')})}>Remove from history</button>}</div><div className="job-location-actions" role="group" aria-label={`Download location for ${j.path}`}>
 {j.state==='completed'&&<button disabled={!!locationPending[j.id]} onClick={()=>void openLocation(j,'reveal')}>{locationPending[j.id]==='reveal'?'Opening...':`Show in ${fileManager}`}</button>}
 <button disabled={!!locationPending[j.id]} onClick={()=>void openLocation(j,'folder')}>{locationPending[j.id]==='folder'?'Opening...':'Open download folder'}</button>
 </div>{locationErrors[j.id]&&<p className="location-error" role="alert">{locationErrors[j.id]}</p>}{j.stats.transferring?.map(f=><div className="file-progress" key={f.name}>{f.name} · {formatBytes(f.bytes)} / {formatBytes(f.size)} · {formatBytes(f.speed)}/s</div>)}<small>{j.detail}</small></section>)}<p className="muted">Retry can skip matching completed files. Partial multithreaded files may restart from byte zero.</p></>}
 <div hidden={tab!=='settings'}><Updates hasDownloads={jobs.some(active)} onAvailable={setUpdateVersion}/></div>
 {tab==='settings'&&<><section className="card"><h2>Transfer settings</h2><p>The transfer engine is included with Remote Desk and managed automatically. No separate download or configuration is needed.</p><div className="fields">{(['transfers','streams','cutoff','chunk','bandwidth'] as const).map(k=><label key={k}>{{transfers:'Concurrent files',streams:'Streams per file',cutoff:'Multithread cutoff',chunk:'Chunk size',bandwidth:'Bandwidth cap (optional)'}[k]}<input value={settings[k]} type={k==='transfers'||k==='streams'?'number':'text'} onChange={e=>setSettings({...settings,[k]:k==='transfers'||k==='streams'?Number(e.target.value):e.target.value})}/></label>)}</div><button disabled={busy} onClick={()=>void action(async()=>{const version=await invoke<string>('set_settings',{settings});setMessage(`Transfer settings saved. ${version}.`)})}>Save transfer settings</button></section>
 <section className="card" id="connection-form"><h2>{editing?'Edit SFTP connection':'Add an SFTP remote'}</h2><p>Passwords are entered in a native system dialog and saved in this computer’s credential store. They never enter this page.</p><div className="fields">{(['label','host','port','user','root'] as const).map(k=><label key={k}>{{label:'Display name',host:'SFTP host / IP',port:'SSH port',user:'Username',root:'Remote root (optional)'}[k]}<input disabled={busy} autoCorrect={k==='user'?'off':undefined} autoCapitalize={k==='user'?'none':undefined} spellCheck={k==='user'?false:undefined} value={draft[k]} type={k==='port'?'number':'text'} onChange={e=>{setDraft({...draft,[k]:k==='port'?Number(e.target.value):e.target.value});if(k==='host'||k==='port'){setHostKey(null);setVerified(false)}}}/></label>)}<label>Authentication<select disabled={busy} value={draft.auth} onChange={e=>setDraft({...draft,auth:e.target.value})}><option value="agent">SSH agent (load key outside app)</option><option value="password">Password in OS credential store</option></select></label></div><div className="row"><p className="muted">Fetch the server’s public host key, then compare its fingerprint with a trusted source.</p><button disabled={busy||!draft.host||!draft.port} onClick={()=>void action(fetchKey)}>{editing?'Fetch / replace host key':'Fetch host key'}</button></div>{hostKey&&<section className="host-key" aria-label="Host key awaiting verification"><strong>Unverified server key</strong><p>{hostKey.host}:{hostKey.port} · {hostKey.algorithm}</p><code>{hostKey.fingerprint}</code><p>Compare this entire fingerprint with your hosting panel, server console, or administrator. Fetching a key does not establish trust.</p></section>}{editing&&!hostKey&&!draft.knownHosts&&<p className="muted">Keeping the saved trusted keys. Changing the host or port requires fetching and verifying its new key. Changing the host, port or username clears the old password.</p>}<details><summary>Import a verified key file instead (optional)</summary><div className="row"><input aria-label="Verified known hosts file" readOnly placeholder="No file needed when fetching a host key" value={draft.knownHosts}/><button disabled={busy} onClick={()=>void action(()=>pick(p=>{setDraft({...draft,knownHosts:p});setHostKey(null);setVerified(false)}))}>Choose known_hosts</button></div></details>{(!!hostKey||!!draft.knownHosts)&&<label className="check"><input type="checkbox" disabled={busy} checked={verified} onChange={e=>setVerified(e.target.checked)}/> I compared the fingerprint with a trusted independent source and it matches. Trust this key for this server.</label>}<div className="row"><button className="primary" disabled={busy||((!editing||!!draft.knownHosts||!!hostKey)&&(!verified||(!draft.knownHosts&&!hostKey)))} onClick={()=>void action(saveRemote)}>{editing?'Save changes':'Save connection'}</button>{editing&&<button disabled={busy} onClick={resetForm}>Cancel editing</button>}</div></section>
 {remotes.map(r=><section className="card" key={r.id}><h2>{r.label||r.host}</h2><p>{r.user}@{r.host}:{r.port} · {r.auth} · root {r.root||'(home)'}</p><div className="connection-actions"><button disabled={busy||!settings.rclone} onClick={()=>void action(async()=>setMessage(await invoke<string>('test_remote',{remoteId:r.id})))}>Test connection</button>{r.auth==='password'&&<button disabled={busy||!settings.rclone} onClick={()=>void action(()=>password(r.id))}>Set / change password</button>}<button disabled={busy} onClick={()=>{setDraft({...r,knownHosts:''});setEditing(true);setVerified(false);setHostKey(null);setDeleting('');document.getElementById('connection-form')?.scrollIntoView({behavior:'smooth'})}}>Edit</button><button className="danger" disabled={busy} onClick={()=>setDeleting(r.id)}>Delete connection</button></div>{deleting===r.id&&<div className="confirm" role="alert"><p>Delete this saved connection and its stored password? Downloaded files and history will stay on this computer.</p><button className="danger" disabled={busy} onClick={()=>void action(()=>removeRemote(r.id))}>Delete connection and password</button> <button disabled={busy} onClick={()=>setDeleting('')}>Keep connection</button></div>}</section>)}</>}
 </main></div>
}
createRoot(document.getElementById('root')!).render(<App/>);
