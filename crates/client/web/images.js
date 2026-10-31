'use strict';

// Keep original pixels out of streamed state and load small previews only when visible.
function attachmentElement(file)
{
    if (file.kind!=='image')
    {
        const chip=document.createElement('span'); chip.className='attachment sent-attachment'; chip.textContent=file.name; return chip;
    }
    const button=document.createElement('button'), image=document.createElement('img'), caption=document.createElement('span');
    button.type='button'; button.className='image-preview'; button.dataset.image=file.url || file.content; button.dataset.imageName=file.name;
    button.setAttribute('aria-label','Enlarge '+file.name); button.title=file.name;
    image.src=file.url?file.url+'/thumbnail':file.content; image.alt=file.name; image.width=112; image.height=84; image.loading='lazy'; image.decoding='async';
    caption.textContent=file.name; button.append(image,caption); return button;
}

// Reuse previews across token updates so their image requests and keyboard focus stay stable.
function renderImageGallery(container,images)
{
    const key=JSON.stringify(images.map(image=>[image.url,image.name]));
    if (container.dataset.images===key) return;
    container.dataset.images=key; container.hidden=!images.length;
    container.replaceChildren(...images.map(attachmentElement));
}

// Native dialog provides focus trapping and Escape; clicks outside the image close the lightbox.
const imageDialog=document.getElementById('image-dialog');
document.addEventListener('click',event=>
{
    const button=event.target.closest('button[data-image]');
    if (!button) return;
    const source=button.dataset.image;
    if (!source.startsWith('/api/media/') && !/^data:image\/(png|jpeg|webp);base64,/u.test(source)) return;
    const image=document.getElementById('image-original'); image.src=source; image.alt=button.dataset.imageName;
    document.getElementById('image-caption').textContent=button.dataset.imageName;
    imageDialog.showModal();
});
document.getElementById('image-close').addEventListener('click',()=>imageDialog.close());
imageDialog.addEventListener('click',event=>{ if (event.target===imageDialog) imageDialog.close(); });
imageDialog.addEventListener('close',()=>document.getElementById('image-original').removeAttribute('src'));
