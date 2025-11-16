<script lang="ts">
  /* Top Level Styles */
  import '../app.css';
  /* Components */
  import TopBar from '$lib/components/TopBar.svelte';
  import Footer from '$lib/components/Footer.svelte';
  import { afterNavigate } from '$app/navigation';
  
  import favicon from '$lib/assets/favicon.svg';
  let { children } = $props();

  // Handle hash scrolling after navigation
  afterNavigate(({ to }) => {
    if (to?.url.hash) {
      const hash = to.url.hash.slice(1);
      const element = document.getElementById(hash);
      
      if (element) {
        // Wait for next tick to ensure DOM is ready
        setTimeout(() => {
          const topbarHeight = 70;
          const elementPosition = element.getBoundingClientRect().top + window.scrollY;
          const offsetPosition = elementPosition - topbarHeight;
          
          window.scrollTo({
            top: offsetPosition,
            behavior: 'instant'
          });
        }, 0);
      }
    }
  });
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

<div class="layout">
  <TopBar />
  <div class="inner">
    <div class="content">
      {@render children()}
    </div>
  </div>
  <Footer />
</div>

<style>
  .layout {
    display: grid;
    grid-template-rows: auto 1fr;
  }
  .inner {
    grid-template-columns: auto 1fr;
  }
</style>
