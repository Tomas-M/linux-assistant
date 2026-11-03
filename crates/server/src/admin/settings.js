'use strict';
let backendSettings;
const settingsGroups=[
    {page:'general',title:'General',fields:[['public_url','Public URL','url'],['free_credits','Welcome credits','number']]},
    {page:'appearance',title:'Client appearance',hint:'Clients fetch these settings once at startup. Restart a client after changing its branding or default theme. A full logo replaces both the square logo and name in the header.',fields:[['client.name','Assistant name'],['client.default_theme','Default theme','theme'],['client.assistant_image','Assistant image','image'],['client.logo_image','Logo before the name','image'],['client.logo_full','Full header logo','image'],['client.favicon','Favicon','image']]},
    {page:'appearance',title:'New chat quick actions',hint:'Buttons appear below the new-chat heading in this order. Clicking fills the prompt without sending it. Leave this list empty to hide the buttons. Restart clients after changing these actions.',fields:[['client.quick_actions','Quick actions','quick-actions']]},
    {page:'ai',title:'AI and token prices',hint:'Rates are credits per million tokens. New tasks use the current prices; existing tasks retain their admitted rates.',fields:[['model','Model'],['reasoning_effort','Reasoning level','select'],['pricing.input_credits_per_million','Input credits / 1M tokens','rate'],['pricing.cached_input_credits_per_million','Cached input credits / 1M tokens','rate'],['pricing.output_credits_per_million','Output credits / 1M tokens','rate']]},
    {page:'ai',title:'Image generation',hint:'GPT Image 2 is available through connected ChatGPT and API accounts. The default 21.1 credits per completed image uses OpenAI’s $0.211 high-quality 1024 × 1024 output estimate. Native ChatGPT usage has no image-token counters, so this is a configurable estimate, not a measured provider bill. Text-token charges are added and the whole task is rounded once. Failed or cancelled images are not charged.',fields:[['image_credits_per_image','Credits per completed image','rate']]},
    {page:'ai',title:'Assistant context',hint:'Applies to all conversations, including account tests, and survives context compaction. Changes apply to the next request.',fields:[['assistant_context','Context and instructions','textarea']]},
    {page:'email',title:'Email and SMTP',provider:'email',hint:'Gmail: use smtp.gmail.com, port 465, your full email and a Google app password (Google Account → Security → App passwords). SMTP uses implicit TLS. SMTP also sends ownership-confirmation codes for social sign-in.',fields:[['email_enabled','Enable email sign-in','checkbox'],['smtp_host','SMTP hostname'],['smtp_port','SMTP port','number'],['smtp_username','SMTP username'],['smtp_password','SMTP password','password'],['email_from','Sender name and email address']]},
    {page:'sign-in',title:'Google sign-in',provider:'google',link:'https://console.cloud.google.com/auth/overview',hint:'Google Cloud → Google Auth Platform: create a project, set Branding and External audience, then Clients → Create client → Web application. Register the callback below. Copy Client ID and Client secret. Use only openid, email and profile scopes; publish the audience for general users.',fields:[['google_enabled','Enable Google sign-in','checkbox'],['google_client_id','Client ID'],['google_client_secret','Client secret','password']]},
    {page:'sign-in',title:'Microsoft sign-in',provider:'microsoft',link:'https://entra.microsoft.com/',hint:'Microsoft Entra → App registrations → New registration. Choose any organizational directory AND personal Microsoft accounts. Add the callback below as Web. Copy Application (client) ID. Certificates & secrets → New client secret: copy Value, not Secret ID. Replace it before its expiry. First-time linking needs an email ownership code, so configure SMTP too.',fields:[['microsoft_enabled','Enable Microsoft sign-in','checkbox'],['microsoft_client_id','Application (client) ID'],['microsoft_client_secret','Client secret value','password']]},
    {page:'sign-in',title:'GitHub sign-in',provider:'github',link:'https://github.com/settings/applications/new',hint:'GitHub → Settings → Developer settings → OAuth Apps → New OAuth App. Set name, homepage to your Public URL, and the callback below. Register application, copy Client ID and generate a client secret. Device Flow can stay off. No repository permissions are requested.',fields:[['github_enabled','Enable GitHub sign-in','checkbox'],['github_client_id','Client ID'],['github_client_secret','Client secret','password']]},
    {page:'payments',title:'Stripe',hint:'Stripe → Product catalog: create a one-time USD Price for each credit pack. Developers / Workbench → API keys: copy your secret key. Add the webhook below for checkout.session.completed and checkout.session.async_payment_succeeded; copy its signing secret. Use keys and prices from the same Stripe mode.',fields:[['stripe_secret','Stripe secret API key','password'],['stripe_webhook_secret','Webhook signing secret','password']]}
];
const settingsPlaceholders={
    'client.name':'Slax Assistant',public_url:'https://ai.slax.org',free_credits:'100',model:'Provider default; e.g. gpt-6.1-sol',
    assistant_context:'Describe your product, users, preferred guidance and relevant background. Leave blank for no additional context.',
    'pricing.input_credits_per_million':'200','pricing.cached_input_credits_per_million':'10','pricing.output_credits_per_million':'1000',
    smtp_host:'smtp.gmail.com',smtp_port:'465',smtp_username:'tomas@slax.org',smtp_password:'Gmail app password or SMTP password',email_from:'Slax AI <tomas@slax.org>',
    google_client_id:'123456789-example.apps.googleusercontent.com',google_client_secret:'Client secret from Google Cloud',
    microsoft_client_id:'Application (client) ID from Microsoft Entra',microsoft_client_secret:'Secret Value, not Secret ID',
    github_client_id:'Client ID from your GitHub OAuth App',github_client_secret:'Client secret from your GitHub OAuth App',
    stripe_secret:'sk_live_…',stripe_webhook_secret:'whsec_…'
};

const settingsPages=[['general','General'],['appearance','Client appearance'],['ai','AI and context'],['email','Email'],['sign-in','Social sign-in'],['payments','Stripe and credit packs'],['security','Admin password'],['backup','Backup and restore']];
document.getElementById('settings-nav').innerHTML=settingsPages.map(([key,label])=>'<a href="#settings/'+key+'">'+label+'</a>').join('');

// Measure only a visible textarea so hidden settings pages never collapse its content height.
function resizeAssistantContext()
{
    const input=document.getElementById('setting-assistant_context');
    if (!input || !input.getClientRects().length) return;
    input.style.height='auto';
    input.style.height=(input.scrollHeight+input.offsetHeight-input.clientHeight)+'px';
}
window.addEventListener('resize',resizeAssistantContext);

// Navigation changes views without discarding unsaved settings or reloading sensitive credentials.
function renderAdminPage()
{
    document.body.classList.remove('admin-initializing');
    const [requested,subpage]=location.hash.slice(1).split('/');
    const sections=[...document.querySelectorAll('main>section')];
    const page=sections.some(section=>section.id===requested)?requested:'accounts';
    const setting=settingsPages.some(([key])=>key===subpage)?subpage:'general';
    for (const section of sections) section.hidden=section.id!==page;
    for (const link of document.querySelectorAll('.topbar nav a'))
    {
        if (link.hash.slice(1)===page) link.setAttribute('aria-current','page');
        else link.removeAttribute('aria-current');
    }
    for (const link of document.querySelectorAll('#settings-nav a'))
    {
        if (link.hash==='#settings/'+setting) link.setAttribute('aria-current','page');
        else link.removeAttribute('aria-current');
    }
    for (const element of document.querySelectorAll('[data-setting-page]')) element.hidden=element.dataset.settingPage!==setting;
    document.getElementById('backend-settings').hidden=setting==='backup';
    document.getElementById('admin-page-title').textContent={accounts:'AI accounts',diagnostics:'Diagnostics',usage:'Usage',users:'Users',payments:'Payments',settings:'Backend settings'}[page];
    requestAnimationFrame(resizeAssistantContext);
    if (page==='usage') requestAnimationFrame(()=>window.dispatchEvent(new Event('resize')));
}
window.addEventListener('hashchange',renderAdminPage);
renderAdminPage();

// Remove stale validity messages when any part of a dependent setting is corrected.
function clearSettingsError()
{
    for (const input of document.querySelectorAll('#backend-settings [aria-invalid="true"]'))
    {
        input.removeAttribute('aria-invalid'); input.removeAttribute('aria-describedby'); input.setCustomValidity('');
    }
    document.getElementById('settings-field-error')?.remove();
    const status=document.getElementById('settings-result');
    if (status.classList.contains('field-error')) status.textContent='';
    status.classList.remove('field-error');
}

// Keep rejected drafts intact and bring the precise setting, including a credit pack row, into view.
function showSettingsError(message,field)
{
    clearSettingsError();
    const pack=/^credit_packs\.(\d+)\.(price_id|usd|credits)$/u.exec(field || '');
    let input=document.getElementById('setting-'+field);
    if (field==='new_password') input=document.getElementById('new-password');
    if (field==='confirmation') input=document.getElementById('confirm-password');
    if (pack) input=document.querySelectorAll('#pack-rows .pack-row')[Number(pack[1])]?.querySelector('[data-pack="'+pack[2]+'"]');
    const status=document.getElementById('settings-result');
    const label=input?.labels[0]?.textContent.trim();
    status.textContent=(pack?'Credit pack '+(Number(pack[1])+1)+' · ':'')+(label?label+': ':'')+message;
    status.classList.add('field-error');
    if (!input) return;
    const detail=document.createElement('small');
    detail.id='settings-field-error'; detail.className='field-error'; detail.textContent=message;
    input.insertAdjacentElement('afterend',detail);
    input.setAttribute('aria-invalid','true'); input.setAttribute('aria-describedby',detail.id); input.setCustomValidity(message);
    const page=input.closest('[data-setting-page]')?.dataset.settingPage;
    if (page) { location.hash='settings/'+page; renderAdminPage(); }
    input.focus({preventScroll:true}); input.scrollIntoView({block:'center'});
}

// Keep callback examples aligned with the Public URL draft without inserting user input as HTML.
function renderCallbacks()
{
    const origin=document.getElementById('setting-public_url').value.replace(/\/+$/u,'');
    for (const code of document.querySelectorAll('[data-callback]')) code.textContent=origin+code.dataset.callback;
}

// Secrets are represented only by saved-value indicators; their contents never return from the backend.
async function loadSettings()
{
    const result=await api('settings');
    backendSettings=result.settings;
    document.getElementById('settings-fields').innerHTML=settingsGroups.map(group=>'<fieldset data-setting-page="'+group.page+'"><legend>'+escapeHtml(group.title)+'</legend>'+
        (group.hint?'<p class="hint">'+escapeHtml(group.hint)+(group.link?' <a target="_blank" rel="noopener noreferrer" href="'+group.link+'">Open provider console</a>':'')+'</p>':'')+
        (group.provider && group.provider!=='email'?'<p class="hint">Callback URL: <code data-callback="/auth/'+group.provider+'/callback"></code></p>':'')+
        (group.title==='Stripe'?'<p class="hint">Webhook URL: <code data-callback="/webhooks/stripe"></code></p>':'')+
        '<div class="settings-grid">'+group.fields.map(([key,label,type='text'])=>
        {
            const value=key.split('.').reduce((value,key)=>value[key],backendSettings);
            if (type==='quick-actions') return '<div class="setting-wide"><div id="quick-action-rows"></div><button type="button" data-add-quick-action>Add quick action</button></div>';
            if (type==='image') return '<div class="brand-upload" data-image-field="'+key+'"><label for="setting-'+key+'">'+escapeHtml(label)+'</label><div class="brand-preview"><img alt="'+escapeHtml(label)+' preview"'+(value?' src="'+escapeHtml(value)+'"':' hidden')+'></div><input id="setting-'+key+'" type="file" data-image="'+key.slice(7)+'" accept="image/png,image/jpeg,image/webp'+(key==='client.favicon' ? ',.ico,image/x-icon,image/vnd.microsoft.icon' : '')+'"><button type="button" class="quiet" data-clear-image="'+key.slice(7)+'"'+(!value?' disabled':'')+'>Remove image</button><small class="hint">'+(key==='client.favicon' ? 'ICO files are uploaded and served unchanged, with all embedded sizes' : 'PNG, JPEG or WebP · at most 4096 × 4096 pixels')+' · up to 2 MiB</small></div>';
            if (type==='checkbox') return '<label class="setting-switch"><input id="setting-'+key+'" data-setting="'+key+'" type="checkbox"'+(value?' checked':'')+'>'+escapeHtml(label)+'</label>';
            const secret=type==='password',saved=secret && result.saved_secrets.includes(key);
            const placeholder=saved?'Saved; leave blank to keep':settingsPlaceholders[key] || '';
            let input;
            if (type==='textarea') input='<textarea id="setting-'+key+'" data-setting="'+key+'" rows="8" placeholder="'+escapeHtml(placeholder)+'">'+escapeHtml(value??'')+'</textarea>';
            else if (type==='select' || type==='theme') input='<select id="setting-'+key+'" data-setting="'+key+'">'+(type==='theme'?['light','dark']:['','none','minimal','low','medium','high','xhigh','max','ultra','persistent']).map(option=>'<option value="'+option+'"'+((value || '')===option?' selected':'')+'>'+(type==='theme'?(option==='dark'?'Dark':'Light'):(option || 'Provider default'))+'</option>').join('')+'</select>';
            else input='<input id="setting-'+key+'" data-setting="'+key+'" type="'+(type==='rate'?'number':type)+'"'+(type==='rate'?' min="0" step="0.000001" required':type==='number'?' min="'+(key==='free_credits'?'0':'1')+'" max="'+(key==='smtp_port'?'65535':'1000000000')+'" step="1" required':'')+' value="'+escapeHtml(secret?'':value??'')+'" placeholder="'+escapeHtml(placeholder)+'"'+(secret?' autocomplete="new-password"':'')+(['public_url','smtp_host'].includes(key)?' required':'')+(key==='model'?' maxlength="200"':'')+'>';
            return '<div'+(type==='textarea'?' class="setting-wide"':'')+'><label for="setting-'+key+'">'+escapeHtml(label)+'</label>'+input+(key==='email_from'?'<small class="hint">Use an email address, e.g. '+escapeHtml('Slax AI <tomas@slax.org>')+'. A name alone is not valid.</small>':'')+(saved?'<label class="setting-switch"><input type="checkbox" data-clear-secret="'+key+'">Clear saved secret</label>':'')+'</div>';
        }).join('')+'</div></fieldset>').join('');
    document.getElementById('setting-public_url').addEventListener('input',renderCallbacks);
    renderCallbacks();
    renderCreditPacks();
    renderQuickActions();
    renderAdminPage();
}

// Keep action labels and prompts in the same ordered settings array used by the client.
function renderQuickActions()
{
    document.getElementById('quick-action-rows').innerHTML=backendSettings.client.quick_actions.map((action,index)=>'<div class="quick-action-row"><label>Button text<input id="setting-client.quick_actions.'+index+'.label" data-action-field="label" maxlength="100" required value="'+escapeHtml(action.label)+'"></label><label>Prompt<textarea id="setting-client.quick_actions.'+index+'.prompt" data-action-field="prompt" rows="3" required>'+escapeHtml(action.prompt)+'</textarea></label><button type="button" class="quiet" data-remove-quick-action="'+index+'" aria-label="Remove quick action"><svg aria-hidden="true"><use href="#icon-remove"/></svg></button></div>').join('');
}
document.getElementById('settings-fields').addEventListener('input',event=>
{
    const input=event.target.closest('[data-action-field]');
    if (!input) return;
    const row=input.closest('.quick-action-row');
    backendSettings.client.quick_actions[[...row.parentElement.children].indexOf(row)][input.dataset.actionField]=input.value;
});
document.getElementById('settings-fields').addEventListener('click',event=>
{
    const button=event.target.closest('[data-add-quick-action],[data-remove-quick-action]');
    if (!button) return;
    clearSettingsError();
    const adding=button.hasAttribute('data-add-quick-action');
    if (adding) backendSettings.client.quick_actions.push({label:'',prompt:''});
    else backendSettings.client.quick_actions.splice(Number(button.dataset.removeQuickAction),1);
    renderQuickActions();
    if (adding) document.querySelector('#quick-action-rows .quick-action-row:last-child input').focus();
});

// Pack rows are explicit operator configuration; their order determines the client button order.
function renderCreditPacks()
{
    document.getElementById('pack-rows').innerHTML=backendSettings.credit_packs.map((pack,index)=>'<div class="pack-row"><label>Stripe Price ID<input data-pack="price_id" placeholder="price_…" maxlength="200" value="'+escapeHtml(pack.price_id)+'" required></label><label>Price in USD<input data-pack="usd" placeholder="2.99" type="number" min="0.01" step="0.01" value="'+escapeHtml(pack.usd)+'" required></label><label>Credits<input data-pack="credits" placeholder="100" type="number" min="1" max="1000000000" step="1" value="'+pack.credits+'" required></label><button type="button" class="quiet" data-remove-pack="'+index+'" aria-label="Remove credit pack"><svg aria-hidden="true"><use href="#icon-remove"/></svg></button></div>').join('');
}
document.getElementById('add-pack').addEventListener('click',()=>
{
    clearSettingsError();
    backendSettings.credit_packs.push({price_id:'',usd:'',credits:100});
    renderCreditPacks();
    document.querySelector('#pack-rows .pack-row:last-child input').focus();
});
document.getElementById('pack-rows').addEventListener('input',event=>
{
    const input=event.target.closest('[data-pack]');
    if (!input) return;
    const row=input.closest('.pack-row'),index=[...row.parentElement.children].indexOf(row);
    backendSettings.credit_packs[index][input.dataset.pack]=input.dataset.pack==='credits'?Number(input.value):input.value;
});
document.getElementById('pack-rows').addEventListener('click',event=>
{
    const button=event.target.closest('[data-remove-pack]');
    if (!button) return;
    clearSettingsError();
    backendSettings.credit_packs.splice(Number(button.dataset.removePack),1);
    renderCreditPacks();
});

// Uploads become validated base64 settings; no local paths are sent to other users.
document.getElementById('settings-fields').addEventListener('change',async event=>
{
    const input=event.target.closest('[data-image]');
    if (!input || !input.files[0]) return;
    const file=input.files[0],container=input.closest('.brand-upload');
    try
    {
        if (file.size>2*1024*1024) throw new Error('Use an image up to 2 MiB.');
        // ICO MIME types vary by browser; recognize its file signature and preserve the original bytes.
        const header=new Uint8Array(await file.slice(0,4).arrayBuffer());
        const favicon=input.dataset.image==='favicon',icon=favicon && header.join(',')==='0,0,1,0';
        if (!icon && !['image/png','image/jpeg','image/webp'].includes(file.type)) throw new Error(favicon?'Choose a PNG, JPEG, WebP or ICO image.':'Choose a PNG, JPEG or WebP image.');
        let content=await new Promise((resolve,reject)=>
        {
            const reader=new FileReader(); reader.onload=()=>resolve(reader.result); reader.onerror=()=>reject(reader.error); reader.readAsDataURL(file);
        });
        if (icon) content='data:image/vnd.microsoft.icon;base64,'+content.slice(content.indexOf(',')+1);
        if (!icon)
        {
            const image=new Image(); image.src=content; await image.decode();
            if (image.naturalWidth>4096 || image.naturalHeight>4096) throw new Error('Use an image of at most 4096 × 4096 pixels.');
        }
        backendSettings.client[input.dataset.image]=content;
        const preview=container.querySelector('img'); preview.src=content; preview.hidden=false;
        container.querySelector('button').disabled=false;
        clearSettingsError();
    }
    catch (error) { showSettingsError(error.message,'client.'+input.dataset.image); }
    finally { input.value=''; }
});
document.getElementById('settings-fields').addEventListener('click',event=>
{
    const button=event.target.closest('[data-clear-image]');
    if (!button) return;
    backendSettings.client[button.dataset.clearImage]=null;
    const preview=button.closest('.brand-upload').querySelector('img'); preview.removeAttribute('src'); preview.hidden=true;
    button.disabled=true; clearSettingsError();
});

document.getElementById('backend-settings').addEventListener('input',clearSettingsError);
document.getElementById('backend-settings').addEventListener('input',event=>{ if (event.target.id==='setting-assistant_context') resizeAssistantContext(); });

document.getElementById('backend-settings').addEventListener('submit',async event=>
{
    event.preventDefault();
    clearSettingsError();
    const button=document.getElementById('save-settings'),status=document.getElementById('settings-result');
    const password=document.getElementById('new-password').value,confirmation=document.getElementById('confirm-password').value;
    if (password!==confirmation) { showSettingsError('Enter the new password identically twice.','confirmation'); return; }
    const settings=structuredClone(backendSettings);
    for (const input of document.querySelectorAll('[data-setting]'))
    {
        const key=input.dataset.setting;
        if (key==='image_credits_per_image') { settings[key]=input.value; continue; }
        if (key.startsWith('pricing.')) { settings.pricing[key.slice(8)]=input.value; continue; }
        if (key.startsWith('client.')) { settings.client[key.slice(7)]=input.value; continue; }
        if (input.type==='checkbox') settings[key]=input.checked;
        else if (input.type==='number') settings[key]=Number(input.value);
        else settings[key]=input.value || null;
    }
    button.disabled=true; status.textContent='Saving…';
    try
    {
        await api('settings',{settings,clear_secrets:[...document.querySelectorAll('[data-clear-secret]:checked')].map(input=>input.dataset.clearSecret),new_password:password || null});
        document.getElementById('new-password').value=''; document.getElementById('confirm-password').value='';
        await loadSettings();
        status.textContent='Saved. Settings are active for new requests.';
        refresh();
    }
    catch (error) { showSettingsError(error.message,error.field); }
    finally { button.disabled=false; }
});
loadSettings().catch(error=>{document.getElementById('settings-result').textContent=error.message;});

// Serialize settings transfers with Save so a restore cannot race a draft submission in this tab.
function setTransferBusy(busy)
{
    for (const id of ['backup-settings','restore-settings','save-settings']) document.getElementById(id).disabled=busy;
}

document.getElementById('backup-settings').addEventListener('click',async()=>
{
    if (document.getElementById('save-settings').disabled) return;
    const status=document.getElementById('backup-status');
    setTransferBusy(true); status.textContent='Preparing backup…';
    try
    {
        const backup=await api('settings/backup');
        const url=URL.createObjectURL(new Blob([JSON.stringify(backup,null,2)+'\n'],{type:'application/json;charset=utf-8'})),link=document.createElement('a');
        link.href=url; link.download='linux-assistant-settings.json'; link.click(); setTimeout(()=>URL.revokeObjectURL(url),1000);
        status.textContent='Backup download started. It includes private credentials; keep it in a safe place.';
    }
    catch (error) { status.textContent=error.message; }
    finally { setTransferBusy(false); }
});

document.getElementById('restore-settings').addEventListener('click',()=>
{
    if (!document.getElementById('save-settings').disabled) document.getElementById('restore-file').click();
});
document.getElementById('restore-file').addEventListener('change',async event=>
{
    const file=event.target.files[0],status=document.getElementById('backup-status');
    event.target.value='';
    if (!file || document.getElementById('save-settings').disabled) return;
    setTransferBusy(true); status.textContent='Reading backup…';
    try
    {
        if (file.size>16*1024*1024) throw new Error('The settings backup exceeds the 16 MiB upload limit.');
        let backup;
        try { backup=JSON.parse(await file.text()); } catch { throw new Error('This file is not valid JSON.'); }
        if (!backup?.settings || !Array.isArray(backup.accounts)) throw new Error('Choose a settings backup downloaded from this application.');
        if (!confirm('Replace saved settings and AI accounts with this backup ('+backup.accounts.length+' accounts)? Public URL: '+backup.settings.public_url+'. Unsaved edits will be discarded. The current admin password, users, credits and conversation history will be kept.')) { status.textContent='Restore cancelled.'; return; }
        status.textContent='Restoring settings and AI accounts…';
        await api('settings/restore',backup);
        clearSettingsError();
        document.getElementById('new-password').value=''; document.getElementById('confirm-password').value='';
        await loadSettings(); await refresh();
        status.textContent='Settings and AI accounts restored. The admin password is unchanged.';
    }
    catch (error) { status.textContent=error.message; }
    finally { setTransferBusy(false); }
});
