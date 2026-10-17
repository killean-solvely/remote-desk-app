import React, {useState} from 'react';
import {invoke} from '@tauri-apps/api/core';

export type Bookmark={id:string;remoteId:string;label:string;path:string;needsReview:boolean;revision:string};
type Connection={id:string;label:string;host:string;port:number;user:string;root:string};
export function BookmarkGroup({remote,bookmarks,currentPath,disabled,onOpen,onSaved,onRemoved}:{
 remote:Connection;bookmarks:Bookmark[];currentPath:string|null;disabled:boolean;
 onOpen:(b:Bookmark)=>void;onSaved:(b:Bookmark)=>void;onRemoved:(id:string)=>void;
}){
 const [editing,setEditing]=useState(''),[name,setName]=useState(''),[pending,setPending]=useState(false),[error,setError]=useState('');
 async function change(fn:()=>Promise<void>){setPending(true);setError('');try{await fn()}catch(e){setError(String(e))}finally{setPending(false)}}
 return <div className="bookmarks" role="group" aria-label={`Bookmarks for ${remote.label||remote.host}`}>
 {!bookmarks.length&&<small className="bookmark-empty">No bookmarks yet</small>}
 {bookmarks.map(b=><div className="bookmark" key={b.id}>
  <div className="bookmark-row"><button className={`bookmark-link ${currentPath===b.path?'chosen':''}`} title={b.path||'Connection root'} disabled={disabled||pending||b.needsReview} onClick={()=>onOpen(b)}><span aria-hidden="true">☆</span><span>{b.label}</span></button><button className="bookmark-edit" disabled={disabled||pending} aria-label={`Edit bookmark ${b.label}`} onClick={()=>{setEditing(editing===b.id?'':b.id);setName(b.label);setError('')}}>{b.needsReview?'Review':'Edit'}</button></div>
  {b.needsReview&&<small className="bookmark-review">Connection changed — review required</small>}
  {editing===b.id&&<form className="bookmark-form" onSubmit={e=>{e.preventDefault();void change(async()=>{onSaved(await invoke<Bookmark>('rename_bookmark',{id:b.id,label:name}));setEditing('')})}}>
   <label>Bookmark name<input aria-label={`Bookmark name for ${b.label}`} value={name} maxLength={120} disabled={pending} onChange={e=>setName(e.target.value)} autoCorrect="off" autoCapitalize="none" spellCheck={false}/></label>
   <small>Relative folder: <code>{b.path||'(root)'}</code></small>
   {b.needsReview&&<div className="bookmark-review-target"><p>This bookmark will use:</p><code>{remote.user}@{remote.host}:{remote.port}</code><small>Connection root: <code>{remote.root||'(home directory)'}</code></small><p>Confirm that the relative folder above belongs on this connection. Confirming does not connect.</p><button type="button" disabled={pending} onClick={()=>void change(async()=>{onSaved(await invoke<Bookmark>('confirm_bookmark',{id:b.id,revision:b.revision}));setEditing('')})}>Confirm target</button></div>}
   <div className="bookmark-form-actions"><button disabled={pending||!name.trim()}>Save name</button><button type="button" disabled={pending} onClick={()=>setEditing('')}>Cancel</button><button type="button" className="danger" disabled={pending} onClick={()=>void change(async()=>{await invoke('delete_bookmark',{id:b.id});onRemoved(b.id);setEditing('')})}>Remove bookmark</button></div>
  </form>}
 </div>)}
 {error&&<p role="alert" className="location-error">{error}</p>}
 </div>;
}
