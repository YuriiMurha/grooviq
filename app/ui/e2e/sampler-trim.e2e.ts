import { test, expect, project, soundsFor } from "./fixture";

for (const [frames,start,end] of [[1,0.2,0.7],[2,0.6,0.4]]) {
  test.describe(`explicit ${frames}-frame repair`,()=>{
    const p=project(["Tiny"]);
    p.channels[0].frames=frames;
    p.channels[0].sampler={start:Math.fround(start),end:Math.fround(end),loop_start:Math.fround(0.37),reverse:1,loop:1,tune:-3};
    test.use({seed:{project:p,files:soundsFor(["Tiny"])}});
    test("accepted pair and exact Undo preserve unrelated parameters",async({app,mock},info)=>{
      await app.setViewportSize({width:900,height:560});
      await app.getByRole("button",{name:"Edit the Tiny sampler"}).click();
      const dialog=app.getByRole("dialog",{name:"Tiny sampler"});
      const repair=dialog.getByRole("button",{name:"Use whole sample"});
      const before=await mock.eval("m=>m.view().channels[0].sampler");
      await expect(repair).toBeEnabled();
      await app.screenshot({path:info.outputPath("damaged.png")});
      expect(await mock.eval("m=>m.view().channels[0].sampler")).toEqual(before);
      await repair.focus();await app.keyboard.press("Enter");
      await expect.poll(()=>mock.eval("m=>[m.view().channels[0].sampler.start,m.view().channels[0].sampler.end]")).toEqual([0,1]);
      const undo=dialog.getByRole("button",{name:"Undo reset"});
      await expect(undo).toBeEnabled();
      // An unrelated accepted edit is retained when the exact original pair returns.
      await dialog.locator('[data-param="reverse"]').click();
      await expect.poll(()=>mock.eval("m=>m.view().channels[0].sampler.reverse")).toBe(0);
      await expect(undo).toBeVisible();
      await undo.click();
      await expect.poll(()=>mock.eval("m=>m.view().channels[0].sampler")).toEqual({...before as object,reverse:0});
      expect(await mock.eval("m=>m.trimInverseCount()")).toBe(0);
      await expect(repair).toBeEnabled();
      await app.screenshot({path:info.outputPath("restored.png")});
    });
    test("queue refusal preserves exact model then accepts retry",async({app,mock})=>{
      await app.getByRole("button",{name:"Edit the Tiny sampler"}).click();
      const repair=app.getByRole("button",{name:"Use whole sample"});
      await expect(repair).toBeEnabled();
      const before=await mock.eval("m=>m.view().channels[0].sampler");
      await mock.eval('m=>m.refuseTrim("Engine queue is full")');
      await repair.click();
      await expect.poll(async()=>(await mock.calls("sampler_trim_reset")).length).toBe(1);
      await expect(repair).toBeEnabled();
      expect(await mock.eval("m=>m.view().channels[0].sampler")).toEqual(before);
      expect(await mock.eval("m=>m.trimInverseCount()")).toBe(0);
      await repair.click();
      await expect(app.getByRole("button",{name:"Undo reset"})).toBeEnabled();
    });
  });
}

test.describe("repair identity fences",()=>{
  const p=project(["Tiny"]);
  p.channels[0].frames=2;p.channels[0].sampler={start:Math.fround(0.6),end:Math.fround(0.4)};
  test.use({seed:{project:p,files:{...soundsFor(["Tiny"]),"/replacement.grooviq":{kind:"project",project:p}}}});
  test("intervening accepted trim edits away and back retire the dedicated offer",async({app,mock})=>{
    await app.getByRole("button",{name:"Edit the Tiny sampler"}).click();
    await app.getByRole("button",{name:"Use whole sample"}).click();
    const undo=app.getByRole("button",{name:"Undo reset"});
    await expect(undo).toBeEnabled();
    const dialog=app.getByRole("dialog",{name:"Tiny sampler"});
    const start=dialog.locator('[data-param="start"]');
    await start.focus();await app.keyboard.press("ArrowRight");await app.keyboard.press("Home");
    await expect(undo).toHaveCount(0);
    await expect.poll(()=>mock.eval("m=>m.view().channels[0].sampler.start")).toBe(0);
    expect(await mock.eval("m=>m.trimState(1).undo")).toBeNull();
  });
  for (const replace of ["new","open"] as const) {
    test(`late accepted reply after ${replace} cannot offer an inverse in replacement`,async({app,mock})=>{
      await app.getByRole("button",{name:"Edit the Tiny sampler"}).click();
      await expect(app.getByRole("button",{name:"Use whole sample"})).toBeEnabled();
      await mock.eval("m=>m.holdTrimResponse()");
      await app.getByRole("button",{name:"Use whole sample"}).click();
      await expect.poll(()=>mock.eval("m=>m.view().channels[0].sampler.start")).toBe(0);
      if(replace==="open")await mock.eval('m=>m.queueDialog("/replacement.grooviq")');
      await app.keyboard.press(replace==="new"?"ControlOrMeta+n":"ControlOrMeta+o");
      await mock.eval("m=>m.releaseTrimResponse()");
      await app.getByRole("dialog",{name:"Save changes?"}).getByRole("button",{name:"Don't save"}).click();
      await expect.poll(async()=>(await mock.calls(replace==="new"?"project_new":"project_open")).length).toBe(1);
      await expect(app.getByRole("button",{name:"Undo reset"})).toHaveCount(0);
      if(replace==="open"){
        await expect.poll(()=>mock.eval("m=>m.view().channels[0].sampler.start")).toBe(Math.fround(0.6));
        await app.getByRole("button",{name:"Edit the Tiny sampler"}).click();
        await expect(app.getByRole("button",{name:"Undo reset"})).toHaveCount(0);
      }
    });
  }
});

test.describe("sampler native-shaped capability contract",()=>{
  const p=project(["Tiny"]);p.channels[0].frames=2;p.channels[0].sampler={start:Math.fround(0.6),end:Math.fround(0.4)};
  test.use({seed:{project:p,files:soundsFor(["Tiny"])}});
  test("stale inverse after same-path relink and late acknowledgment remains unusable",async({app,mock})=>{
    const call=<T,>(command:string,args:Record<string,unknown>)=>app.evaluate(([command,args])=>(window as unknown as {__TAURI_INTERNALS__:{invoke:(command:string,args:unknown)=>Promise<unknown>}}).__TAURI_INTERNALS__.invoke(command,args),[command,args] as const) as Promise<T>;
    type View={stamp:string;undo:string|null;start:number;end:number};
    const initial=await call<View>("sampler_trim_view",{channel:1});
    const reply=await call<{state:View}>("sampler_trim_reset",{channel:1,stamp:initial.stamp});
    await call("channel_set_sample",{channel:1,path:"/qa/sounds/Tiny.wav",generation:0});
    const before=await mock.eval("m=>m.view().channels[0].sampler");
    const refused=await call<{status:string}>("sampler_trim_undo",{channel:1,stamp:reply.state.stamp,token:reply.state.undo});
    expect(refused.status).toBe("refused");
    expect(await mock.eval("m=>m.view().channels[0].sampler")).toEqual(before);
    expect((await call<View>("sampler_trim_view",{channel:1})).undo).toBeNull();
  });
});


test.describe("read-only repair observation during a legal gesture", () => {
  const p = project(["Tiny"]);
  p.channels[0].frames = 48_000;
  p.channels[0].sampler = { start: 0.1, end: 0.9 };
  test.use({ seed: { project: p, files: soundsFor(["Tiny"]) } });
  test("an unrelated accepted edit and trim refresh preserve a held marker drag", async ({ app, mock }) => {
    await app.getByRole("button", { name: "Edit the Tiny sampler" }).click();
    const dialog = app.getByRole("dialog", { name: "Tiny sampler" });
    const marker = dialog.locator('[data-param="start"]');
    await expect.poll(async () => (await mock.calls("sampler_trim_view")).length).toBeGreaterThan(0);
    const box = (await marker.boundingBox())!;
    const x = box.x + box.width / 2;
    const y = box.y + box.height / 2;
    await app.mouse.move(x, y);
    await app.mouse.down();
    await app.mouse.move(x + 20, y, { steps: 4 });
    await expect.poll(async () => (await mock.calls("channel_param_live")).length).toBeGreaterThan(0);
    await dialog.locator('[data-param="reverse"]').focus();
    await app.keyboard.press("Enter");
    await expect.poll(() => mock.eval("m=>m.view().channels[0].sampler.reverse")).toBe(1);
    await expect.poll(async () => (await mock.calls("sampler_trim_view")).length).toBeGreaterThan(1);
    await app.mouse.move(x + 80, y, { steps: 4 });
    await app.mouse.up();
    await expect.poll(() => mock.eval("m=>m.view().channels[0].sampler.start")).toBeGreaterThan(0.15);
    expect(await mock.eval("m=>m.view().channels[0].sampler.end")).toBe(0.9);
    expect(await mock.eval("m=>m.view().channels[0].sampler.reverse")).toBe(1);
  });
});
