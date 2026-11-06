package com.zebra.eventinjectionservice;

import android.view.InputEvent;

interface IEventInjectionService {

    boolean authenticate();

    boolean injectInputEvent(in InputEvent event,int mode);

}
