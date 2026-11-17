import { createContext, useContext } from "react";

/** The scale a device window is drawn at (it scales itself to fit the app window), for content
 * that draws pixels itself (a canvas). */
export const DeviceScale = createContext(1);
export const useDeviceScale = () => useContext(DeviceScale);
