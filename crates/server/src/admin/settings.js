'use strict';
let backendSettings;
const settingsGroups=[
    {title:'General',fields:[['public_url','Public URL','url'],['free_credits','Welcome credits','number']]},
    {title:'AI and token prices',hint:'Rates are credits per million tokens. New tasks use the current prices; existing tasks retain their admitted rates.',fields:[['model','Model'],['reasoning_effort','Reasoning level','select'],['pricing.input_credits_per_million','Input credits / 1M tokens','rate'],['pricing.cached_input_credits_per_million','Cached input credits / 1M tokens','rate'],['pricing.output_credits_per_million','Output credits / 1M tokens','rate']]},
    {title:'Email and SMTP',provider:'email',hint:'Gmail: use smtp.gmail.com, port 465, your full email and a Google app password (Google Account → Security → App passwords). SMTP uses implicit TLS. SMTP also sends ownership-confirmation codes for social sign-in.',fields:[['email_enabled','Enable email sign-in','checkbox'],['smtp_host','SMTP hostname'],['smtp_port','SMTP port','number'],['smtp_username','SMTP username'],['smtp_password','SMTP password','password'],['email_from','Sender, e.g. Slax AI <tomas@slax.org>']]},
    {title:'Google sign-in',provider:'google',link:'https://console.cloud.google.com/auth/overview',hint:'Google Cloud → Google Auth Platform: create a project, set Branding and External audience, then Clients → Create client → Web application. Register the callback below. Copy Client ID and Client secret. Use only openid, email and profile scopes; publish the audience for general users.',fields:[['google_enabled','Enable Google sign-in','checkbox'],['google_client_id','Client ID'],['google_client_secret','Client secret','password']]},
    {title:'Microsoft sign-in',provider:'microsoft',link:'https://entra.microsoft.com/',hint:'Microsoft Entra → App registrations → New registration. Choose any organizational directory AND personal Microsoft accounts. Add the callback below as Web. Copy Application (client) ID. Certificates & secrets → New client secret: copy Value, not Secret ID. Replace it before its expiry. First-time linking needs an email ownership code, so configure SMTP too.',fields:[['microsoft_enabled','Enable Microsoft sign-in','checkbox'],['microsoft_client_id','Application (client) ID'],['microsoft_client_secret','Client secret value','password']]},
    {title:'GitHub sign-in',provider:'github',link:'https://github.com/settings/applications/new',hint:'GitHub → Settings → Developer settings → OAuth Apps → New OAuth App. Set name, homepage to your Public URL, and the callback below. Register application, copy Client ID and generate a client secret. Device Flow can stay off. No repository permissions are requested.',fields:[['github_enabled','Enable GitHub sign-in','checkbox'],['github_client_id','Client ID'],['github_client_secret','Client secret','password']]},
    {title:'Stripe',hint:'Stripe → Product catalog: create a one-time USD Price for each credit pack. Developers / Workbench → API keys: copy your secret key. Add the webhook below for checkout.session.completed and checkout.session.async_payment_succeeded; copy its signing secret. Use keys and prices from the same Stripe mode.',fields:[['stripe_secret','Stripe secret API key','password'],['stripe_webhook_secret','Webhook signing secret','password']]}
];

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
            if (type==='checkbox') return '<label class="setting-switch"><input id="setting-'+key+'" data-setting="'+key+'" type="checkbox"'+(value?' checked':'')+'>'+label+'</label>';
            const secret=type==='password',saved=secret && result.saved_secrets.includes(key);
            let input;
            if (type==='select') input='<select id="setting-'+key+'" data-setting="'+key+'">'+['','none','minimal','low','medium','high','xhigh','max','ultra','persistent'].map(option=>'<option value="'+option+'"'+((value || '')===option?' selected':'')+'>'+(option || 'Provider default')+'</option>').join('')+'</select>';
            else input='<input id="setting-'+key+'" data-setting="'+key+'" type="'+(type==='rate'?'number':type)+'"'+(type==='rate'?' min="0" step="0.000001" required':type==='number'?' min="'+(['free_credits'].includes(key)?'0':'1')+'" step="1" required':'')+' value="'+escapeHtml(secret?'':value??'')+'"'+(secret?' autocomplete="new-password" placeholder="'+(saved?'Saved; leave blank to keep':'Not configured')+'"':'')+(key==='public_url'?' required':'')+'>';
            return '<div><label for="setting-'+key+'">'+label+'</label>'+input+(saved?'<label class="setting-switch"><input type="checkbox" data-clear-secret="'+key+'">Clear saved secret</label>':'')+'</div>';
        }).join('')+'</div></fieldset>').join('');
    document.getElementById('setting-public_url').addEventListener('input',renderCallbacks);
    renderCallbacks();
    renderCreditPacks();
}

// Pack rows are explicit operator configuration; their order determines the client button order.
function renderCreditPacks()
{
    document.getElementById('pack-rows').innerHTML=backendSettings.credit_packs.map((pack,index)=>'<div class="pack-row"><label>Stripe Price ID<input data-pack="price_id" value="'+escapeHtml(pack.price_id)+'" required></label><label>Price in USD<input data-pack="usd" type="number" min="0.01" step="0.01" value="'+escapeHtml(pack.usd)+'" required></label><label>Credits<input data-pack="credits" type="number" min="1" step="1" value="'+pack.credits+'" required></label><button type="button" class="quiet" data-remove-pack="'+index+'" aria-label="Remove credit pack"><svg aria-hidden="true"><use href="#icon-remove"/></svg></button></div>').join('');
}
document.getElementById('add-pack').addEventListener('click',()=>
{
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
    backendSettings.credit_packs.splice(Number(button.dataset.removePack),1);
    renderCreditPacks();
});

document.getElementById('backend-settings').addEventListener('submit',async event=>
{
    event.preventDefault();
    const button=document.getElementById('save-settings'),status=document.getElementById('settings-result');
    const password=document.getElementById('new-password').value,confirmation=document.getElementById('confirm-password').value;
    if (password!==confirmation) { status.textContent='Enter the new password identically twice.'; return; }
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
    catch (error) { status.textContent=error.message; }
    finally { button.disabled=false; }
});
loadSettings().catch(error=>{document.getElementById('settings-result').textContent=error.message;});
