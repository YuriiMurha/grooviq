import { expect, it } from "vitest";
import { trimRepairState } from "./trimRepair";
it("distinguishes damaged, immovable valid, recoverable short and unknown geometry", () => {
  const base = { start:0.2,end:0.7,loopStart:0,reverse:false,frames:1 };
  expect(trimRepairState(base)).toEqual({damaged:true,stuck:true,wholeFrame:false});
  expect(trimRepairState({...base,frames:2,start:0.6,end:0.4}).stuck).toBe(true);
  expect(trimRepairState({...base,start:0,end:1})).toEqual({damaged:false,stuck:false,wholeFrame:true});
  expect(trimRepairState({...base,frames:2,start:0.1,end:0.2})).toEqual({damaged:true,stuck:false,wholeFrame:false});
  expect(trimRepairState({...base,frames:null})).toEqual({damaged:false,stuck:false,wholeFrame:false});
  expect(trimRepairState({...base,frames:0}).damaged).toBe(false);
});
