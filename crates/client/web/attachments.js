'use strict';
let draftAttachments=[], attaching=false;

// FileReader preserves the original encoded bytes without re-encoding the image.
function imageData(file)
{
    return new Promise((resolve,reject)=>
    {
        const reader=new FileReader(); reader.onload=()=>resolve(reader.result); reader.onerror=()=>reject(reader.error); reader.readAsDataURL(file);
    });
}

// Decode only local image data, including a draft restored from the durable queue.
async function imageBitmap(content)
{
    const [header,encoded]=content.split(',');
    const bytes=Uint8Array.from(atob(encoded),character=>character.charCodeAt(0));
    return createImageBitmap(new Blob([bytes],{type:header.slice(5,-7)}));
}

// Preserve original bytes and dimensions for picker, drop, and clipboard input.
async function addAttachments(files)
{
    if (attaching || elements.prompt.disabled) return;
    attaching=true; renderComposer();
    try
    {
        const added=[];
        for (const file of files)
        {
            if (new TextEncoder().encode(file.name).length>255 || /[\u0000-\u001f\u007f]/u.test(file.name)) throw new Error('The attachment filename is too long or contains control characters.');
            const mime=file.type || ({png:'image/png',jpg:'image/jpeg',jpeg:'image/jpeg',webp:'image/webp'}[file.name.split('.').at(-1).toLowerCase()] || '');
            if (/^image\/(png|jpeg|webp)$/u.test(mime))
            {
                const bitmap=await createImageBitmap(file);
                try { added.push({kind:'image',name:file.name,content:await imageData(new Blob([file],{type:mime})),width:bitmap.width,height:bitmap.height}); }
                finally { bitmap.close(); }
            }
            else
            {
                const textType=file.type.startsWith('text/') || /\.(txt|md|log|csv|tsv|json|jsonl|xml|html|css|js|ts|jsx|tsx|rs|py|sh|ps1|toml|yaml|yml|ini|conf|sql|c|h|cpp|hpp|java|php|svg)$/iu.test(file.name) || !file.name.includes('.');
                if (!textType) throw new Error(file.name+': supported files are PNG, JPEG, WebP, and UTF-8 text/code. PDFs, Office files and archives are not supported yet.');
                const content=new TextDecoder('utf-8',{fatal:true}).decode(await file.arrayBuffer());
                if (content.includes('\0')) throw new Error(file.name+': this is a binary file, not UTF-8 text.');
                added.push({kind:'text',name:file.name,content});
            }
        }
        draftAttachments.push(...added); steerId=null; elements.status.textContent='';
    }
    catch (error) { elements.status.textContent=error.message; }
    finally { attaching=false; elements['attachment-input'].value=''; renderComposer(); }
}

// This deliberately rough estimate excludes history, cached context, responses, and command follow-ups.
async function confirmInputCost(text,attachments)
{
    let tokens=new TextEncoder().encode(text).length/3;
    for (const file of attachments)
    {
        if (file.kind==='text') tokens+=new TextEncoder().encode(file.content).length/3;
        else
        {
            if (!file.width || !file.height)
            {
                const bitmap=await imageBitmap(file.content); file.width=bitmap.width; file.height=bitmap.height; bitmap.close();
            }
            tokens+=Math.ceil(file.width/32)*Math.ceil(file.height/32)*1.2;
        }
    }
    tokens=Math.ceil(tokens);
    if (tokens<=2000) return true;
    const rate=state.account?.input_credits_per_million;
    const credits=Number.isFinite(rate)?Math.max(1,Math.ceil(tokens*rate/1000000)):null;
    const price=credits!==null?'Approximately '+credits.toLocaleString()+' '+(credits===1?'credit':'credits')+' for this new input.':'The current credit rate is unavailable; the cost cannot be estimated.';
    elements['input-cost-description'].textContent='This message and its attachments may use around '+tokens.toLocaleString()+' input tokens. '+price+' This is a rough estimate, not the total task price. Conversation context, the answer and command follow-ups add to it; the final task cost is rounded up once. Large inputs can use a lot of credits. Send anyway?';
    const dialog=elements['input-cost-dialog']; dialog.returnValue=''; dialog.showModal();
    return new Promise(resolve=>
    {
        const controller=new AbortController();
        const finish=accepted=>{controller.abort(); dialog.close(); resolve(accepted);};
        dialog.addEventListener('submit',event=>{event.preventDefault(); finish(event.submitter.value==='send');},{signal:controller.signal});
        dialog.addEventListener('cancel',event=>{event.preventDefault(); finish(false);},{signal:controller.signal});
    });
}

// Display file names safely; deleting a chip only changes the unsent draft.
function renderAttachments()
{
    elements['attach'].disabled=attaching || elements.prompt.disabled;
    elements['attachments'].hidden=!draftAttachments.length;
    elements['attachment-note'].hidden=!draftAttachments.length && !attaching;
    elements['attachment-note'].textContent=attaching?'Preparing attachments…':'Original attachments · Large inputs require confirmation before sending';
    const key=JSON.stringify(draftAttachments.map(item=>[item.name,item.kind,item.content.length]));
    if (elements.attachments.dataset.key===key) return;
    elements.attachments.dataset.key=key;
    elements.attachments.replaceChildren();
    draftAttachments.forEach((file,index)=>
    {
        const chip=document.createElement('span'), name=document.createElement('span'), remove=document.createElement('button');
        chip.className='attachment'; name.textContent=file.name; name.title=file.kind==='image'?file.name+' · '+file.width+'×'+file.height+' · Original':file.name;
        remove.type='button'; remove.dataset.attachment=index; remove.setAttribute('aria-label','Remove '+file.name); setIcon(remove,'close');
        chip.append(name,remove); elements.attachments.append(chip);
    });
}
