import React, {useEffect, useRef, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {listen} from '@tauri-apps/api/event';

type Status={enabled:boolean;permission:'notRequested'|'firstUse'|'granted'|'denied'|'unavailable';detail:string;lastError:string|null;testPending:boolean};
export function Notifications(){
 const [status,setStatus]=useState<Status|null>(null),[busy,setBusy]=useState(false),[error,setError]=useState(''),[message,setMessage]=useState('');
 const pending=useRef(false),sequence=useRef(0),mounted=useRef(true);
 async function refresh(){
  if(pending.current)return;const ticket=++sequence.current;
  try{const next=await invoke<Status>('get_notification_status');if(mounted.current&&ticket===sequence.current){setStatus(next);setError('')}}
  catch(e){if(mounted.current&&ticket===sequence.current)setError(String(e))}
 }
 useEffect(()=>{
  mounted.current=true;void refresh();
  const focus=()=>void refresh();window.addEventListener('focus',focus);
  const off=listen('notification_status_changed',()=>void refresh());
  return()=>{mounted.current=false;sequence.current++;window.removeEventListener('focus',focus);void off.then(f=>f()).catch(()=>{})};
 },[]);
 async function change(command:'set_notifications'|'test_notification',enabled?:boolean){
  if(pending.current)return;pending.current=true;sequence.current++;setBusy(true);setError('');setMessage('');
  try{const next=await invoke<Status>(command,command==='set_notifications'?{enabled}:undefined);if(mounted.current){setStatus(next);setMessage(command==='test_notification'?'Test scheduled. Switch to another app now; it will be sent in 5 seconds. Focus settings may silence it.':next.enabled?(next.permission==='granted'?'Background download alerts enabled.':'Preference saved. Review notification availability below.'):'Desktop notifications turned off.')}}
  catch(e){if(mounted.current)setError(String(e))}
  finally{pending.current=false;if(mounted.current)setBusy(false)}
 }
 return <section className="card notification-settings" aria-labelledby="notification-heading">
  <h2 id="notification-heading">Download notifications</h2>
  <p>Get a desktop alert when a download finishes or fails while Remote Desk is in the background. Remote Desk must stay open.</p>
  <label className="check"><input type="checkbox" checked={status?.enabled??false} disabled={busy||!status} onChange={e=>void change('set_notifications',e.target.checked)}/> Desktop notifications</label>
  <p className="muted">Messages say only “Download completed” or “Download failed.” Filenames, server details and error logs are kept in the app. Cancellations and old history do not send alerts.</p>
  <p role="status">{status?status.detail:'Loading notification settings...'}</p>
  <div className="connection-actions"><button disabled={busy} onClick={()=>void refresh()}>{status?'Check permission':'Retry notification settings'}</button>
   {status?.enabled&&status.permission==='notRequested'&&<button disabled={busy} onClick={()=>void change('set_notifications',true)}>Allow notifications</button>}
   <button disabled={busy||!status?.enabled||!['granted','firstUse'].includes(status.permission)||status.testPending} onClick={()=>void change('test_notification')}>{status?.testPending?'Test scheduled...':'Send test in 5 seconds'}</button></div>
  {message&&<p className="notification-message" role="status">{message}</p>}
  {(error||status?.lastError)&&<p role="alert" className="location-error">{error||status?.lastError}</p>}
 </section>;
}
