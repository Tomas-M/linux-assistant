'use strict';
let backendSettings;
const settingsGroups=[
    {title:'General',fields:[['public_url','Public URL','url'],['free_credits','Welcome credits','number']]},
    {title:'AI and token prices',hint:'Rates are credits per million tokens. New tasks use the current prices; existing tasks retain their admitted rates.',fields:[['model','Model'],['reasoning_effort','Reasoning level','select'],['pricing.input_credits_per_million','Input credits / 1M tokens','rate'],['pricing.cached_input_credits_per_million','Cached input credits / 1M tokens','rate'],['pricing.output_credits_per_million','Output credits / 1M tokens','rate']]},
    {title:'Email and SMTP',provider:'email',hint:'Gmail: use smtp.gmail.com, port 465, your full email and a Google app password (Google Account → Security → App passwords). SMTP uses implicit TLS. SMTP also sends ownership-confirmation codes for social sign-in.',fields:[['email_enabled','Enable email sign-in','checkbox'],['smtp_host','SMTP hostname'],['smtp_port','SMTP port','number'],['smtp_username','SMTP username'],['smtp_password','SMTP password','password'],['email_from','Sender name and email address']]},
    {title:'Google sign-in',provider:'google',link:'https://console.cloud.google.com/auth/overview',hint:'Google Cloud → Google Auth Platform: create a project, set Branding and External audience, then Clients → Create client → Web application. Register the callback below. Copy Client ID and Client secret. Use only openid, email and profile scopes; publish the audience for general users.',fields:[['google_enabled','Enable Google sign-in','checkbox'],['google_client_id','Client ID'],['google_client_secret','Client secret','password']]},
    {title:'Microsoft sign-in',provider:'microsoft',link:'https://entra.microsoft.com/',hint:'Microsoft Entra → App registrations → New registration. Choose any organizational directory AND personal Microsoft accounts. Add the callback below as Web. Copy Application (client) ID. Certificates & secrets → New client secret: copy Value, not Secret ID. Replace it before its expiry. First-time linking needs an email ownership code, so configure SMTP too.',fields:[['microsoft_enabled','Enable Microsoft sign-in','checkbox'],['microsoft_client_id','Application (client) ID'],['microsoft_client_secret','Client secret value','password']]},
    {title:'GitHub sign-in',provider:'github',link:'https://github.com/settings/applications/new',hint:'GitHub → Settings → Developer settings → OAuth Apps → New OAuth App. Set name, homepage to your Public URL, and the callback below. Register application, copy Client ID and generate a client secret. Device Flow can stay off. No repository permissions are requested.',fields:[['github_enabled','Enable GitHub sign-in','checkbox'],['github_client_id','Client ID'],['github_client_secret','Client secret','password']]},
    {title:'Stripe',hint:'Stripe → Product catalog: create a one-time USD Price for each credit pack. Developers / Workbench → API keys: copy your secret key. Add the webhook below for checkout.session.completed and checkout.session.async_payment_succeeded; copy its signing secret. Use keys and prices from the same Stripe mode.',fields:[['stripe_secret','Stripe secret API key','password'],['stripe_webhook_secret','Webhook signing secret','password']]}
];
const settingsPlaceholders={
    public_url:'https://ai.slax.org',free_credits:'100',model:'Provider default; e.g. gpt-6.1-sol',
    'pricing.input_credits_per_million':'200','pricing.cached_input_credits_per_million':'10','pricing.output_credits_per_million':'1000',
    smtp_host:'smtp.gmail.com',smtp_port:'465',smtp_username:'tomas@slax.org',smtp_password:'Gmail app password or SMTP password',email_from:'Slax AI <tomas@slax.org>',
    google_client_id:'123456789-example.apps.googleusercontent.com',google_client_secret:'Client secret from Google Cloud',
    microsoft_client_id:'Application (client) ID from Microsoft Entra',microsoft_client_secret:'Secret Value, not Secret ID',
    github_client_id:'Client ID from your GitHub OAuth App',github_client_secret:'Client secret from your GitHub OAuth App',
    stripe_secret:'sk_live_…',stripe_webhook_secret:'whsec_…'
};

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
    document.getElementById('settings-fields').innerHTML=settingsGroups.map(group=>'<fieldset><legend>'+escapeHtml(group.title)+'</legend>'+
        (group.hint?'<p class="hint">'+escapeHtml(group.hint)+(group.link?' <a target="_blank" rel="noopener noreferrer" href="'+group.link+'">Open provider console</a>':'')+'</p>':'')+
        (group.provider && group.provider!=='email'?'<p class="hint">Callback URL: <code data-callback="/auth/'+group.provider+'/callback"></code></p>':'')+
        (group.title==='Stripe'?'<p class="hint">Webhook URL: <code data-callback="/webhooks/stripe"></code></p>':'')+
        '<div class="settings-grid">'+group.fields.map(([key,label,type='text'])=>
        {
            const value=key.startsWith('pricing.')?backendSettings.pricing[key.slice(8)]:backendSettings[key];
            if (type==='checkbox') return '<label class="setting-switch"><input id="setting-'+key+'" data-setting="'+key+'" type="checkbox"'+(value?' checked':'')+'>'+escapeHtml(label)+'</label>';
            const secret=type==='password',saved=secret && result.saved_secrets.includes(key);
            const placeholder=saved?'Saved; leave blank to keep':settingsPlaceholders[key] || '';
            let input;
            if (type==='select') input='<select id="setting-'+key+'" data-setting="'+key+'">'+['','none','minimal','low','medium','high','xhigh','max','ultra','persistent'].map(option=>'<option value="'+option+'"'+((value || '')===option?' selected':'')+'>'+(option || 'Provider default')+'</option>').join('')+'</select>';
            else input='<input id="setting-'+key+'" data-setting="'+key+'" type="'+(type==='rate'?'number':type)+'"'+(type==='rate'?' min="0" step="0.000001" required':type==='number'?' min="'+(key==='free_credits'?'0':'1')+'" max="'+(key==='smtp_port'?'65535':'1000000000')+'" step="1" required':'')+' value="'+escapeHtml(secret?'':value??'')+'" placeholder="'+escapeHtml(placeholder)+'"'+(secret?' autocomplete="new-password"':'')+(['public_url','smtp_host'].includes(key)?' required':'')+(key==='model'?' maxlength="200"':'')+'>';
            return '<div><label for="setting-'+key+'">'+escapeHtml(label)+'</label>'+input+(key==='email_from'?'<small class="hint">Use an email address, e.g. '+escapeHtml('Slax AI <tomas@slax.org>')+'. A name alone is not valid.</small>':'')+(saved?'<label class="setting-switch"><input type="checkbox" data-clear-secret="'+key+'">Clear saved secret</label>':'')+'</div>';
        }).join('')+'</div></fieldset>').join('');
    document.getElementById('setting-public_url').addEventListener('input',renderCallbacks);
    renderCallbacks();
    renderCreditPacks();
}

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

document.getElementById('backend-settings').addEventListener('input',clearSettingsError);

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
        if (key.startsWith('pricing.')) { settings.pricing[key.slice(8)]=input.value; continue; }
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
