import {spawnSync} from 'node:child_process';
import {writeFileSync,mkdirSync} from 'node:fs';
const target=process.argv[2];
const signed=Boolean(process.env.TAURI_SIGNING_PRIVATE_KEY);
if(process.env.REMOTE_DESK_RELEASE_TAG&&!signed)throw Error('Release builds require the updater signing secret.');
mkdirSync('.tools',{recursive:true});
writeFileSync('.tools/updater-build.json',JSON.stringify({bundle:{createUpdaterArtifacts:signed}}));
const r=spawnSync(process.execPath,['node_modules/@tauri-apps/cli/tauri.js','build','--target',target,'--bundles',process.platform==='darwin'?'app,dmg':'nsis','--config','.tools/updater-build.json'],{stdio:'inherit',env:process.env});
process.exit(r.status??1);
