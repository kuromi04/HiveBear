package com.hivebear.app

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private var multicastLock: WifiManager.MulticastLock? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    try {
      val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
      multicastLock = wifi?.createMulticastLock("hivebear_lan_discovery")?.apply {
        setReferenceCounted(true)
        acquire()
      }
    } catch (_: Exception) {}
  }

  override fun onDestroy() {
    try {
      multicastLock?.release()
    } catch (_: Exception) {}
    super.onDestroy()
  }
}
